use anyhow::Result;
use axum::{
    Router, extract::{Json, Path, State}, http::header, response::IntoResponse, routing::{get, post}
};
use clap::Parser;
use reqwest::Client;
use serde::Deserialize;
use steam_current_game::{CurrentGameResponse, GameInfo, ReportData, ServerInfo};
use tokio::sync::RwLock;
use tracing::{error, info, instrument};
use std::{
    collections::HashMap, env, sync::Arc, time::{Duration, Instant}
};

// --- 状态存储 ---
struct ServerState {
    pub boot_id: String,
    pub app_states: Arc<RwLock<HashMap<String, AppState>>>,
    pub name_cache: Arc<RwLock<HashMap<u32, String>>>,
    pub http_client: Client,
    pub api_base_url: String,
    pub names_path: String,
}

#[derive(Clone, Debug)]
struct AppState {
    // 记录当前的 AppID，用于判断是否切换了游戏
    pub current_app_id: u32,
    // 当前显示的游戏名
    pub current_game_name: String,
    // 游戏开始时间
    pub session_start_time: Option<Instant>,

    pub last_updated_at: Instant,
}

impl Default for AppState {
    fn default() -> Self {
        AppState {
            current_app_id: 0,
            current_game_name: "未在游玩".to_string(),
            session_start_time: None,
            last_updated_at: Instant::now(),
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

#[derive(Debug, Parser)]
#[command(name = "SteamCurrentGameServer")]
struct Cli {
    #[arg(long, default_value = "./config/name.json")]
    names: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::INFO)
        .flatten_event(true)
        .with_span_list(false)
        .with_current_span(false)
        .with_target(false)
        .init();

    // 初始化
    let http_client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let cli = Cli::parse();
    let names_path = cli.names;
    info!(path = %names_path, "name.json path configured");

    // 初始化状态
    let shared_state = Arc::new(ServerState {
        boot_id: uuid::Uuid::new_v4().to_string(),
        app_states: Arc::new(RwLock::new(HashMap::new())),
        name_cache: Arc::new(RwLock::new(HashMap::new())),
        http_client,
        api_base_url: "https://store.steampowered.com".to_string(),
        names_path,
    });

    let app = Router::new()
        .route("/{token}", get(root_handler))           // 网页外壳
        .route("/current/{token}", get(current_game_handler)) // 动态 Tag 接口
        .route("/upload", post(upload_handler))  // Client 上报接口
        .route("/upload/{token}/{game_name}", get(upload_redir_handler))  // Client 上报接口
        .with_state(shared_state);

    let listen_addr = env::var("ADDRESS").unwrap_or("0.0.0.0:3000".to_string());
    info!(address = %listen_addr, "Server starting");

    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn upload_redir_handler(
    State(server_state): State<Arc<ServerState>>,
    Path((token, game_name)): Path<(String, String)>
) -> impl IntoResponse {
    let payload = if let Ok(app_id) = game_name.parse::<u32>() {
        ReportData {
            app_id,
            token: token.clone(),
            game_name: None,
        }
    } else {
        ReportData {
            app_id: 114514,
            token: token.clone(),
            game_name: Some(game_name),
        }
    };

    upload_handler(State(server_state), Json(payload)).await
}

// --- [核心逻辑] 处理 Client 上报 ---
#[instrument(
    skip(server_state, payload),
    fields(token = %payload.token, app_id = payload.app_id, game_name = ?payload.game_name))]
async fn upload_handler(
    State(server_state): State<Arc<ServerState>>,
    Json(payload): Json<ReportData>,
) -> impl IntoResponse {
    let request_start_time = Instant::now();

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
        let state = app_states_guard.entry(token.clone()).or_insert_with(|| AppState {
            last_updated_at: request_start_time,
            ..Default::default()
        });

        let needs_update = match payload.game_name {
            // 只要 游戏名 变了就更新
            Some(ref game_name) => state.current_game_name != *game_name,
            None => state.current_app_id != new_app_id,
        };
        if needs_update {
            old_app_id = state.current_app_id;
            old_app_name = state.current_game_name.clone();
            old_start_time = state.session_start_time;
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
    let (game_name, start_time) = if new_app_id == 0 {  // 没有在游玩
        ("未在游玩".to_string(), None)
    } else if let Some(game_name) = payload.game_name {  // 客户端直接提供了游戏名
        if game_name == "0" || game_name == "未在游玩" {
            ("未在游玩".to_string(), None)
        } else {
            (game_name, Some(Instant::now()))
        }
    } else {  // 需要通过 API 获取游戏名
        let name = if let Some(n) = lookup_name_override(&server_state.names_path, new_app_id).await {
            n
        } else {
            // 2.1 先查缓存 (NameCache 有自己的锁，粒度很小)
            let cached_name = {
                let cache = server_state.name_cache.read().await;
                cache.get(&new_app_id).cloned()
            };

            if let Some(n) = cached_name {
                n
            } else {
                // 2.2 缓存没有，发网络请求 (这里是最慢的，但现在是无锁的！)
                match fetch_game_name(
                    &server_state.http_client,
                    &server_state.api_base_url,
                    new_app_id
                ).await {
                    Ok(n) => {
                        server_state.name_cache.write().await.insert(new_app_id, n.clone());
                        n
                    }
                    Err(e) => {
                        error!(error = %e, app_id = new_app_id, "Steam API 请求失败");
                        format!("未知游戏 ({})", new_app_id)
                    }
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
    info!(
        token = %token,
        old.app_id = old_app_id,
        old.app_name = %old_app_name,
        new.app_id = new_app_id,
        new.game_name = %game_name,
        last_session.duration_str = %play_time_str,
        last_session.duration_sec = play_time,
        "用户状态变更" // 这是一个 message 字段
    );

    // --- 第三步：写入数据 (再次获取写锁，极快) ---
    {
        let mut app_states_guard = server_state.app_states.write().await;
        // 这里必须再次获取 entry，因为锁断开了
        let state = app_states_guard.entry(token).or_insert_with(AppState::default);

        if state.last_updated_at > request_start_time {
            info!("检测到过期请求，丢弃更新 (AppID: {})", new_app_id);
            return "Ignored";
        }

        state.last_updated_at = request_start_time;
        state.current_app_id = new_app_id;
        state.current_game_name = game_name;
        state.session_start_time = start_time;
    }

    "OK"
}

async fn current_game_handler(
    State(server_state): State<Arc<ServerState>>,
    Path(token): Path<String>
) -> impl IntoResponse {
    let app_states_guard = server_state.app_states.read().await;

    // 1. 默认响应头 (JSON)
    let headers = [
        (header::CONTENT_TYPE, "application/json; charset=utf-8"),
        (header::CACHE_CONTROL, "no-cache, no-store, must-revalidate"),
        (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
    ];

    // 2. 检查 Token 是否存在
    if app_states_guard.get(&token).is_none() {
        let resp = CurrentGameResponse {
            server: ServerInfo {
                boot_id: server_state.boot_id.clone(),
            },
            info: GameInfo {
                is_running: false,
                signature: "invalid_token".to_string(),
            },
            render: String::new(),
        };
        return (headers, Json(resp));
    }

    let state = app_states_guard.get(&token).unwrap();

    // 3. 判断是否在玩游戏
    if state.current_game_name == "未在游玩" {
        let resp = CurrentGameResponse {
            server: ServerInfo {
                boot_id: server_state.boot_id.clone(),
            },
            info: GameInfo {
                is_running: false,
                // 当没玩游戏时，签名固定为 idle，方便前端去重
                signature: "idle".to_string(),
            },
            render: String::new(),
        };
        return (headers, Json(resp));
    }

    // 4. 正在玩游戏：生成渲染内容
    let status_text = "当前游戏";
    let game_color = "#66C0F4";
    let play_time_str = if let Some(start) = state.session_start_time {
        format_duration(start.elapsed())
    } else {
        "00:00".to_string()
    };

    let html = get_html_template()
        .await
        .replace("%Status%", status_text)
        .replace("%GameName%", &state.current_game_name)
        .replace("%PlayTime%", &play_time_str)
        .replace("%GameNameColor%", game_color);

    // 5. 生成会话签名
    // 使用 "AppID_启动时间戳" 作为唯一标识。
    // 如果 AppID 变了，或者同一个游戏关闭又重开了(时间变了)，签名都会变。
    // 客户端不需要解析这个字符串，只管对比是否相等。
    let signature = if let Some(start) = state.session_start_time {
        format!("{}_{:?}", state.current_app_id, start)
    } else {
        format!("{}", state.current_app_id)
    };

    let resp = CurrentGameResponse {
        server: ServerInfo {
            boot_id: server_state.boot_id.clone(),
        },
        info: GameInfo {
            is_running: true,
            signature,
        },
        render: html,
    };

    (headers, Json(resp))
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

async fn lookup_name_override(path: &str, app_id: u32) -> Option<String> {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(_) => return None,
    };

    match serde_json::from_str::<HashMap<String, String>>(&content) {
        Ok(map) => map.get(&app_id.to_string()).cloned(),
        Err(e) => {
            error!(error = %e, path = %path, "name.json 解析失败，已忽略");
            None
        }
    }
}


async fn get_html_template() -> String {
    include_str!("../../templates/modern.card.html").to_string()
}

// --- 静态页面外壳 (保持不变) ---
async fn root_handler(
    Path(token): Path<String>
) -> impl IntoResponse {
    let html =  include_str!("../../templates/interval-display.main.html");
    let html = html.replace("%token%", &token);
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
}

// --- Steam API 请求 ---
#[instrument(skip(client, base_url), err)]
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
