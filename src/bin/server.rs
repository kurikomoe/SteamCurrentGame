use anyhow::Result;
use axum::{
    Router, extract::{Json, Path, State}, http::header, response::IntoResponse, routing::{get, post}
};
use reqwest::Client;
use serde::Deserialize;
use steam_current_game::ReportData;
use tokio::sync::RwLock;
use std::{
    collections::HashMap, sync::Arc, time::{Duration, Instant}
};

// --- 状态存储 ---
struct ServerState {
    pub app_states: Arc<RwLock<HashMap<String, AppState>>>,
    pub name_cache: Arc<RwLock<HashMap<u32, String>>>,
    pub http_client: Client,
    pub api_base_url: String,
}

#[derive(Clone, Debug)]
struct AppState {
    // 记录当前的 AppID，用于判断是否切换了游戏
    pub current_app_id: u32,
    // 当前显示的游戏名
    pub current_game_name: String,
    // 游戏开始时间
    pub session_start_time: Option<Instant>,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            current_app_id: 0,
            current_game_name: "未在游玩".to_string(),
            session_start_time: None,
        }
    }
}

// Steam API 响应结构
#[derive(Deserialize, Debug)]
struct StoreResponse {
    success: bool,
    data: Option<StoreData>,
}
#[derive(Deserialize, Debug)]
struct StoreData {
    name: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    // 初始化
    let http_client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;

    // 初始化状态
    let shared_state = Arc::new(ServerState {
        app_states: Arc::new(RwLock::new(HashMap::new())),
        name_cache: Arc::new(RwLock::new(HashMap::new())),
        http_client,
        api_base_url: "https://store.steampowered.com".to_string(),
    });

    let app = Router::new()
        .route("/{token}", get(root_handler))           // 网页外壳
        .route("/current/{token}", get(current_game_handler)) // 动态 Tag 接口
        .route("/upload", post(upload_handler))  // Client 上报接口
        .with_state(shared_state);

    let listen_addr = "0.0.0.0:3000";
    println!("Server listening on http://{}", listen_addr);
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

// --- [核心逻辑] 处理 Client 上报 ---
async fn upload_handler(
    State(server_state): State<Arc<ServerState>>,
    Json(payload): Json<ReportData>,
) -> impl IntoResponse {
    let mut old_app_id = 0;
    let mut old_app_name = String::new();

    let mut old_start_time = None;
    let new_app_id = payload.app_id;
    let token = payload.token;

    // --- 第一步：快速检查 (持锁检查，极快) ---
    // 目的：判断是否真的需要更新，如果不需要，立刻返回，不占用锁
    let needs_update = {
        let mut app_states_guard = server_state.app_states.write().await;
        // 如果 token 不存在，这就插入默认值
        let state = app_states_guard.entry(token.clone()).or_insert_with(AppState::default);

        let needs_update = state.current_app_id != new_app_id;
        if needs_update {
            old_app_id = state.current_app_id;
            old_start_time = state.session_start_time;
            old_app_name = if old_app_id != 0 {
                server_state.name_cache.read().await
                    .get(&old_app_id)
                    .cloned()
                    .unwrap_or_else(|| state.current_game_name.clone())
            } else {
                "未在游玩".to_string()
            };
        }
        // 只有 ID 变了才需要后续的耗时操作
        needs_update
    };

    if !needs_update {
        return "OK";
    }

    // --- 第二步：准备数据 (无锁裸奔，耗时操作) ---
    // 这里做网络请求，不持有任何 app_states 的锁
    // 其他用户的 /current 接口现在可以秒开
    let (game_name, start_time) = if new_app_id == 0 {
        ("未在游玩".to_string(), None)
    } else {
        // 2.1 先查缓存 (NameCache 有自己的锁，粒度很小)
        let cached_name = {
            let cache = server_state.name_cache.read().await;
            cache.get(&new_app_id).cloned()
        };

        let name = if let Some(n) = cached_name {
            n
        } else {
            // 2.2 缓存没有，发网络请求 (这里是最慢的，但现在是无锁的！)
            match fetch_game_name(
                &server_state.http_client,
                &server_state.api_base_url,
                new_app_id
            ).await {
                Ok(n) => {
                    // 写入缓存
                    server_state.name_cache.write().await.insert(new_app_id, n.clone());
                    n
                }
                Err(e) => {
                    eprintln!("API Error: {}", e);
                    format!("未知游戏 ({})", new_app_id)
                }
            }
        };
        (name, Some(Instant::now()))
    };
    let (play_time_str, play_time) =
        if let Some(start) = old_start_time {
            (format_duration(start.elapsed()), start.elapsed().as_secs())
        } else {
            ("00:00".to_string(), 0)
        };
    println!("[{}] 状态变更 {} ({}) -> {} ({}), 游戏时长为: {} ({})",
        &token,
        old_app_id, old_app_name,
        new_app_id, game_name,
        play_time_str, play_time);

    // --- 第三步：写入数据 (再次获取写锁，极快) ---
    {
        let mut app_states_guard = server_state.app_states.write().await;
        // 这里必须再次获取 entry，因为锁断开了
        let state = app_states_guard.entry(token).or_insert_with(AppState::default);

        state.current_app_id = new_app_id;
        state.current_game_name = game_name;
        state.session_start_time = start_time;
    }

    "OK"
}

// --- 保持原有的渲染逻辑 ---
async fn current_game_handler(
    State(server_state): State<Arc<ServerState>>,
    Path(token): Path<String>
) -> impl IntoResponse {
    let app_states_guard = server_state.app_states.read().await;
    if app_states_guard.get(&token).is_none() {
        // 未注册的 token，返回空内容
        return (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            String::new(),
        );
    }
    let app_states_guard = server_state.app_states.read().await;
    let state = app_states_guard.get(&token).unwrap();

    // 如果未在游玩，返回空内容
    let content = if state.current_game_name == "未在游玩" {
        String::new()
    } else {
        let status_text = "当前游戏";
        let game_color = "#66C0F4";
        let play_time_str = if let Some(start) = state.session_start_time {
            format_duration(start.elapsed())
        } else {
            "00:00".to_string()
        };

        // 使用你之前调整好的最新 HTML 模板
        get_html_template()
            .await
            .replace("%Status%", status_text)
            .replace("%GameName%", &state.current_game_name)
            .replace("%PlayTime%", &play_time_str)
            .replace("%GameNameColor%", game_color)
    };

    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        content,
    )
}

fn format_duration(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, secs)
    } else {
        format!("{:02}:{:02}", minutes, secs)
    }
}

// --- 静态页面外壳 (保持不变) ---
async fn root_handler(
    Path(token): Path<String>
) -> impl IntoResponse {
    let html = r#"<!DOCTYPE html>
        <html>
        <head>
            <meta charset="utf-8">
            <title>Steam Status Monitor</title>
            <style>
                /* 确保页面背景完全透明 */
                body, html {
                    margin: 0;
                    padding: 0;
                    width: 100%;
                    height: 100%;
                    background-color: transparent;
                    overflow: hidden;
                }

                /* 容器用于放置卡片 */
                #container {
                    width: 100%;
                    height: 100%;
                    display: flex;
                    align-items: center;
                    justify-content: flex-start; /* 靠左对齐 */
                    padding: 40px; /* 给一点边距，防止贴边 */
                    box-sizing: border-box;

                    /* --- 核心动画设置 --- */
                    opacity: 0; /* 默认隐藏 */
                    transition: opacity 0.8s cubic-bezier(0.4, 0, 0.2, 1); /* 平滑的淡入淡出曲线 */

                    /* 保持硬件加速，防止动画模糊 */
                    transform: translateZ(0);
                    will-change: opacity;
                }
            </style>
        </head>
        <body>
            <div id="container"></div>

            <script>
                const API_URL = "/current/%token%";
                const REFRESH_INTERVAL = 1000;
                const container = document.getElementById('container');

                let isVisible = false;

                const updateState = async() => {
                    try {
                        const response = await fetch(API_URL);

                        if (response.ok) {
                            const htmlContent = await response.text();
                            const isPlaying = htmlContent.trim().length > 0;

                            if (isPlaying) {
                                // 状态：正在游玩
                                if (container.innerHTML !== htmlContent) {
                                    container.innerHTML = htmlContent;
                                }

                                if (!isVisible) {
                                    requestAnimationFrame(() => {
                                        container.style.opacity = '1';
                                    });
                                    isVisible = true;
                                }

                            } else {
                                // 状态：未在游玩 (后端返回空)
                                if (isVisible) {
                                    container.style.opacity = '0';
                                    isVisible = false;
                                }
                            }

                        } else {
                            throw new Error("Server error");
                        }
                    } catch (error) {
                        console.warn("Connection lost...", error);
                        if (isVisible) {
                            container.style.opacity = '0';
                            isVisible = false;
                        }
                    }
                }

                setInterval(updateState, REFRESH_INTERVAL);
                updateState();
            </script>
        </body>
        </html>"#;
    let html = html.replace("%token%", &token);
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
}

// --- Steam API 请求 ---
async fn fetch_game_name(client: &Client, base_url: &str, app_id: u32) -> Result<String, String> {
    let url = format!("{}/api/appdetails", base_url);

    let params = [
        ("appids", app_id.to_string()),
        ("l", "schinese".to_string()),
    ];
    let url = reqwest::Url::parse_with_params(&url, &params).unwrap();

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if let Ok(json) = resp.json::<serde_json::Value>().await {
        let id_str = app_id.to_string();
        if let Some(app_data) = json.get(&id_str)
            && let Ok(store_resp) = serde_json::from_value::<StoreResponse>(app_data.clone())
            && store_resp.success
            && let Some(data) = store_resp.data {
                return Ok(data.name);
        }
    }
    Err("API 解析失败".to_string())
}

async fn get_html_template() -> String {
    r##"
    <style>
        /* 全局容器设置 */
        .obs-hidpi-scope {
            font-family: 'Microsoft YaHei', 'SimHei', Arial, sans-serif;
            box-sizing: border-box;

            display: inline-flex;
            flex-direction: row;
            align-items: stretch;
            height: 160px;
            padding: 8px;

            margin: 0;
            line-height: 1;
        }

        /* --- 通用方块基础样式 --- */
        .hud-block {
            display: flex;
            align-items: center;
            justify-content: center;

            /* [2x] 内边距 24px -> 48px */
            padding: 0 48px;

            border-radius: 0;
            background-color: #0a0a0a;

            border: 4px solid #e0e0e0;
            margin-right: -4px;

            position: relative;
        }

        /* --- 1. 状态块 (左侧) --- */
        .block-status {
            background-color: #e0e0e0;
            color: #000000;
            font-weight: 900;

            font-size: 56px;

            text-transform: uppercase;

            min-width: 200px;
            z-index: 3;
        }

        /* --- 2. 游戏名块 (中间) --- */
        .block-game {
            flex: 0 1 auto;

            max-width: 1200px;
            min-width: 400px;

            color: #ffffff;

            font-size: 64px;

            font-weight: bold;
            z-index: 2;

            white-space: nowrap;
            overflow: hidden;
        }

        .game-text {
            overflow: hidden;
            text-overflow: ellipsis;
            /* [2x] 微调 2px -> 4px */
            padding-bottom: 4px;
        }

        /* --- 3. 时间块 (右侧) --- */
        .block-time {
            font-family: 'Consolas', 'Courier New', monospace;
            background-color: #1a1a1a;
            color: #cccccc;

            font-size: 64px;

            min-width: 320px;
            z-index: 1;

            border-right: 12px solid #66c0f4;
        }

        .time-icon {
            font-size: 48px;

            margin-right: 24px;

            color: #66c0f4;
            font-weight: bold;
        }

    </style>

    <div class="obs-hidpi-scope">
        <div class="hud-block block-status">
            %Status%
        </div>

        <div class="hud-block block-game">
            <span class="game-text">%GameName%</span>
        </div>

        <div class="hud-block block-time">
            <span class="time-icon">TIME</span>
            %PlayTime%
        </div>
    </div>
    "##.to_string()
}
