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
    let address = env::var("LISTEN_ADDRESS").unwrap_or_else(|_| "localhost:3000".to_string());

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
        .steam-monitor-scope {
            font-family: 'Microsoft YaHei', 'Segoe UI', Tahoma, sans-serif;
            box-sizing: border-box;
            /* 强制 GPU 渲染，在高分屏下防止文字抖动 */
            transform: translateZ(0);
            -webkit-font-smoothing: antialiased;
        }

        .steam-card {
            display: inline-flex;
            flex-direction: row;
            align-items: center;

            background: linear-gradient(135deg, #171a21 0%, #2a475e 100%);

            /* 宽度限制放大到 4000px */
            max-width: 4000px;

            /* 高度放大: 100 -> 200 */
            height: 200px;

            /* 圆角放大: 16 -> 32 */
            border-radius: 32px;

            /* 内边距放大: 24 -> 48 */
            padding: 0 48px;

            /* 阴影放大 */
            box-shadow: 0 16px 32px rgba(0,0,0,0.4);

            white-space: nowrap;
            overflow: hidden;
        }

        .steam-card span {
            line-height: normal;
        }

        /* 1. 标签：当前游玩 */
        .steam-label {
            /* 字体放大: 28 -> 56 */
            font-size: 56px;
            color: #8F98A0;
            font-weight: 500;
            text-shadow: 0px 4px 4px rgba(0,0,0,0.8); /* 阴影偏移放大 */

            margin-right: 8px; /* 间距放大 */
            flex-shrink: 0;
        }

        /* 外层盒子 */
        .steam-game-name-box {
            display: inline-flex;
            align-items: center;

            flex: 0 1 auto;
            min-width: 0;

            /* 高度放大: 64 -> 128 */
            height: 128px;
            box-sizing: border-box;

            background: rgba(0, 0, 0, 0.35);

            /* 边框放大: 2 -> 4 */
            border: 4px solid rgba(102, 192, 244, 0.25);

            /* 圆角放大: 12 -> 24 */
            border-radius: 24px;

            /* 内边距放大: 20 -> 40 */
            padding: 0 40px;

            /* 左右外边距放大: 16 -> 32 */
            margin: 0 32px;
        }

        /* 内层文本 */
        .steam-game-name-text {
            display: block;
            width: 100%;

            white-space: nowrap;
            overflow: hidden;
            text-overflow: ellipsis;
            text-align: center;

            /* 字体放大: 28 -> 56 */
            font-size: 56px;
            font-weight: bold;
            color: %GameNameColor%;

            /* 发光范围放大: 15 -> 30 */
            text-shadow: 0px 0px 30px rgba(102, 192, 244, 0.5);
        }

        /* 4. 时间色块 */
        .steam-time {
            display: inline-flex;
            align-items: center;
            justify-content: center;

            /* 高度放大: 54 -> 108 */
            height: 108px;

            font-family: 'Consolas', 'Monaco', monospace;

            /* 字体放大: 28 -> 56 */
            font-size: 56px;
            color: #c7d5e0;

            background-color: #1b2838;

            /* 边框放大: 2 -> 4 */
            border: 4px solid #2a3f5a;

            /* 内边距放大: 16 -> 32 */
            padding: 0 32px;

            /* 圆角放大: 8 -> 16 */
            border-radius: 16px;

            /* 字间距放大: 1 -> 2 */
            letter-spacing: 2px;
            flex-shrink: 0;
        }

        .time-icon {
            /* 图标放大: 24 -> 48 */
            font-size: 48px;

            /* 间距放大: 10 -> 20 */
            margin-right: 20px;

            filter: grayscale(100%) opacity(0.7);
        }
    </style>

    <div class="steam-monitor-scope">
        <div class="steam-card">
            <span class="steam-label">%Status%</span>

            <span class="steam-game-name-box">
                <span class="steam-game-name-text">%GameName%</span>
            </span>

            <span class="steam-time">
                <span class="time-icon">⏱</span>
                %PlayTime%
            </span>
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
        let status_text = "正在游玩";
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
                /* 容器用于放置 SVG */
                #container {
                    width: 100%;
                    height: 100%;
                    display: flex;
                    align-items: center;
                    justify-content: flex-start;
                }
            </style>
        </head>
        <body>
            <div id="container"></div>

            <script>
                const API_URL = "/current"; // 相对路径，请求同域下的接口
                const REFRESH_INTERVAL = 1000; // 1秒轮询一次
                const container = document.getElementById('container');

                updateState = async() => {
                    try {
                        const response = await fetch(API_URL);

                        if (response.ok) {
                            const htmlContent = await response.text();

                            // 3. 简单的 Diff：只有内容变了才更新 DOM，避免闪烁
                            if (container.innerHTML !== htmlContent) {
                                container.innerHTML = htmlContent;
                            }
                        } else {
                            throw new Error("Server error");
                        }
                    } catch (error) {
                        // 如果 fetch 失败（网络断了、Rust 进程挂了），
                        // 将内容清空，OBS 上显示为全透明，而不是报错页面。
                        // 等 Rust 重启后，下一次轮询会自动恢复显示。
                        console.warn("Connection lost or server error, clearing display...", error);
                        if (container.innerHTML !== "") {
                            container.innerHTML = "";
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
