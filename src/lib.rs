use serde::{Deserialize, Serialize};


#[derive(Deserialize, Serialize, Debug, Clone)]
#[derive(Default)]
#[serde(default)]
pub struct ReportData {
    pub app_id: u32,
    pub token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_name: Option<String>,
}

#[derive(Serialize)]
pub struct CurrentGameResponse {
    pub server: ServerInfo,
    pub info: GameInfo,
    pub render: String,
}

#[derive(Serialize)]
pub struct ServerInfo {
    pub boot_id: String,
}

#[derive(Serialize)]
pub struct GameInfo {
    // 游戏是否正在运行
    pub is_running: bool,
    // 会话签名：由 "AppID + 启动时间" 组成
    // 客户端只需要判断这个字符串是否变化，不需要关心里面是什么
    pub signature: String,
}
