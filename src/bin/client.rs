use anyhow::Result;
use reqwest::Client;
use steam_current_game::ReportData;
use std::{env, thread, time::Duration};

#[cfg(target_os = "windows")]
use winreg::{enums::*, RegKey};

// --- 通讯协议 ---
// 目标服务器配置
const SERVER_URL: &str = "http://jp.kuriko.moe:3000/upload";

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let server_url = env::var("SERVER_URL").unwrap_or_else(|_| SERVER_URL.to_string());
    println!("Server URL: {}", server_url);

    let token = env::var("API_TOKEN").expect("API_TOKEN 环境变量未设置");

    println!("Steam Monitor Client Started.");
    println!("Target Server: {}", server_url);

    let client = Client::new();
    let mut last_reported_id: Option<u32> = None;

    loop {
        // 1. 获取当前 AppID
        // 0 means no game running or error
        let current_id = get_running_app_id().unwrap_or(0);

        // 2. 检查是否需要上报
        // 逻辑：如果 ID 变了，或者这是第一次运行，就上报
        let need_report = match last_reported_id {
            Some(last_id) => last_id != current_id,
            None => true,
        };

        if need_report {
            println!("状态变化检测: {:?} -> {}", last_reported_id, current_id);

            // 构造 Payload
            let payload = ReportData {
                app_id: current_id,
                token: token.clone(),
            };

            // 发送 POST 请求
            match client.post(SERVER_URL).json(&payload).send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        println!("上报成功: AppID {}", current_id);
                        last_reported_id = Some(current_id);
                    } else {
                        eprintln!("上报失败: HTTP {}", resp.status());
                    }
                }
                Err(e) => {
                    eprintln!("网络错误: {}", e);
                }
            }
        }

        // 每 1 秒轮询一次
        thread::sleep(Duration::from_secs(1));
    }
}

// --- Windows 注册表读取 ---
#[cfg(target_os = "windows")]
fn get_running_app_id() -> Result<u32> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(steam) = hkcu.open_subkey("Software\\Valve\\Steam")
        && let Ok(running_id) = steam.get_value::<u32, _>("RunningAppID")
        && running_id > 0 {
            return Ok(running_id);
    }
    Err(anyhow::anyhow!("Failed to get running app id"))
}

// --- 非 Windows 占位符 ---
#[cfg(not(target_os = "windows"))]
fn get_running_app_id() -> Option<u32> {
    // 可以在这里写死一个 ID 用于非 Windows 环境测试
    None
}
