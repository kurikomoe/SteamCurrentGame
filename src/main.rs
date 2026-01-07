use anyhow::Result;
use axum::{Router, extract::State, response::IntoResponse, routing::get};
use dashmap::DashMap;
use reqwest::Client;
use reqwest::header;
use serde::Deserialize;
use std::time::Instant;
use std::{env, sync::Arc, time::Duration};
use tokio::sync::RwLock;

// 仅在 Windows 下引入 winreg
#[cfg(target_os = "windows")]
use winreg::{RegKey, enums::*};

// --- 全局状态结构体 ---
struct AppState {
    /// 监听地址
    address: String,
    /// 当前显示的游戏名 (Web 接口读取，后台任务写入)
    current_game_name: Arc<RwLock<String>>,

    // 新增：当前游戏开始的时间 (如果是 None 表示未在游玩)
    session_start_time: Arc<RwLock<Option<Instant>>>,

    /// 缓存: AppID -> 中文名 (避免重复请求 API)
    name_cache: DashMap<u32, String>,
    // HTTP 客户端
    http_client: Client,
    /// Steam API 基础地址 (可配置)
    api_base_url: String,
}

// --- Steam API 响应结构 (简化版) ---
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
    // 1. 初始化配置
    // 尝试读取 .env 文件（如果有）
    dotenv::dotenv().ok();

    // 获取监听地址，默认为 localhost:3000，可通过环境变量覆盖
    let address = env::var("LISTEN_ADDRESS").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    // 获取 API 地址，默认为官方地址，可通过环境变量覆盖
    let api_base_url =
        env::var("STEAM_API_URL").unwrap_or_else(|_| "https://store.steampowered.com".to_string());

    println!("Starting Steam Monitor...");
    println!("Using Steam Store API: {}", api_base_url);

    let http_client = Client::builder().timeout(Duration::from_secs(5)).build()?;

    // 2. 初始化共享状态
    let shared_state = Arc::new(AppState {
        address,

        current_game_name: Arc::new(RwLock::new("未在游玩".to_string())),
        session_start_time: Arc::new(RwLock::new(None)),

        name_cache: DashMap::new(),

        http_client,
        api_base_url,
    });

    // 3. 启动后台监控任务 (每秒读取注册表)
    let monitor_state = shared_state.clone();
    tokio::spawn(async move {
        monitor_loop(monitor_state).await;
    });

    // 4. 启动 Web 服务器
    let app_state = shared_state.clone();
    let app = Router::new()
        .route("/", get(root_handler))
        .route("/current", get(current_game_handler))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(&shared_state.address).await?;
    println!("Web server listening on http://{}", &shared_state.address);
    axum::serve(listener, app).await?;

    Ok(())
}

fn format_duration(elapsed: std::time::Duration) -> String {
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

// --- 1. 数据接口：返回 SVG 片段或空字符串 ---
async fn current_game_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let game_name = state.current_game_name.read().await;
    let start_time = state.session_start_time.read().await;

    // 逻辑：如果 "未在游玩"，返回空内容（透明）；否则返回 SVG
    let content = if *game_name == "未在游玩" {
        String::new()
    } else {
        let status_text = "当前游戏";
        let game_color = "#66C0F4"; // Steam 亮蓝

        let play_time_str = if let Some(start) = *start_time {
            format_duration(start.elapsed())
        } else {
            "00:00".to_string()
        };

        let template = get_html_template().await;

        template
            .replace("%Status%", status_text)
            .replace("%GameName%", &game_name)
            .replace("%PlayTime%", &play_time_str)
            .replace("%GameNameColor%", game_color)
    };

    // 返回响应：必须包含禁止缓存的 Header，否则 OBS 可能会一直显示旧状态
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"),
            (header::PRAGMA, "no-cache"),
            (header::EXPIRES, "0"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        content,
    )
}

// --- 2. 静态外壳：负责加载 JS 并轮询 ---
async fn root_handler() -> impl IntoResponse {
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
                const API_URL = "/current";
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
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        html,
    )
}

// --- 后台监控循环 ---
async fn monitor_loop(state: Arc<AppState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    loop {
        interval.tick().await;

        // 获取当前运行的 AppID
        let app_id = get_running_app_id();

        #[derive(Debug)]
        enum StateUpdateResult {
            None,
            Success(String),
            ErrorWithMsg(String),
        }
        let mut new_game_name = StateUpdateResult::None;

        match app_id {
            Some(id) => {
                // 1. 先查缓存
                if let Some(cached_name) = state.name_cache.get(&id) {
                    new_game_name = StateUpdateResult::Success(cached_name.clone());
                } else {
                    let name = fetch_game_name(&state.http_client, &state.api_base_url, id).await;

                    // 写入缓存，只记录成功的名称
                    if let Ok(ref name) = name {
                        println!("已记录 AppID: {} -- {}", id, name);
                        new_game_name = StateUpdateResult::Success(name.clone());
                        state.name_cache.insert(id, name.clone());
                    } else if let Err(e) = name{
                        println!("AppID: {}，请求失败：{}", id, e);
                        new_game_name = StateUpdateResult::ErrorWithMsg(e);
                    }
                }
            }
            None => {
                new_game_name = StateUpdateResult::Success("未在游玩".to_string());
            }
        }

        let mut name_guard = state.current_game_name.write().await;
        let mut time_guard = state.session_start_time.write().await;

        // 如果 new_game_name 为 None，表示读取失败，不更新状态
        // 如果名字变了 (比如从 "未在游玩" -> "黑神话"，或者从 "CSGO" -> "DOTA2")
        match new_game_name {
            StateUpdateResult::Success(new_game_name) => {
                if *name_guard != new_game_name {
                    *name_guard = new_game_name.clone(); // 更新名字

                    if new_game_name == "未在游玩" {
                        *time_guard = None; // 停止计时
                    } else {
                        *time_guard = Some(Instant::now()); // 开始新计时
                    }
                }
            }
            StateUpdateResult::ErrorWithMsg(err_msg) => {
                if *name_guard != err_msg {
                    *name_guard = err_msg;
                }
            }
            StateUpdateResult::None => { /* 不更新状态 */ }
        }
    }
}

// --- 获取 Steam 游戏名 (带网络请求) ---
async fn fetch_game_name(client: &Client, base_url: &str, app_id: u32) -> Result<String, String> {
    let url = format!("{}/api/appdetails", base_url);

    let params = [
        ("appids", app_id.to_string()),
        ("l", "schinese".to_string()),
    ];
    let url = reqwest::Url::parse_with_params(&url, &params).unwrap();

    let resp = client.get(url).send().await;

    match resp {
        Ok(response) => {
            if let Ok(json) = response.json::<serde_json::Value>().await {
                // 解析路径: { "2358720": { "success": true, "data": { "name": "..." } } }
                let id_str = app_id.to_string();
                if let Some(app_data) = json.get(&id_str)
                    && let Ok(store_resp) =
                        serde_json::from_value::<StoreResponse>(app_data.clone())
                    && store_resp.success
                    && let Some(data) = store_resp.data
                {
                    return Ok(data.name);
                }
            }
            // 失败回退
            Err(format!("未知游戏 ({})", app_id))
        }
        Err(e) => {
            eprintln!("网络错误: {}", e);
            Err(format!("网络错误 ({})", app_id))
        }
    }
}

// --- Windows 注册表读取 ---
#[cfg(target_os = "windows")]
fn get_running_app_id() -> Option<u32> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    // 路径：HKEY_CURRENT_USER\Software\Valve\Steam
    // 键值：RunningAppID
    if let Ok(steam) = hkcu.open_subkey("Software\\Valve\\Steam")
        && let Ok(running_id) = steam.get_value::<u32, _>("RunningAppID")
        && running_id > 0
    {
        return Some(running_id);
    }

    None
}

// --- 非 Windows 系统存根 (防止编译报错) ---
#[cfg(not(target_os = "windows"))]
fn get_running_app_id() -> Option<u32> {
    println!("Not on Windows, simulation mode.");
    None
}
