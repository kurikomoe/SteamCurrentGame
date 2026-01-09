use serde::{Deserialize, Serialize};


#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ReportData {
    pub app_id: u32,
    pub token: String,
}

#[derive(Serialize)]
pub struct CurrentGameResponse {
    pub info: GameInfo,
    pub render: String,
}

#[derive(Serialize)]
pub struct GameInfo {
    // 游戏是否正在运行
    pub is_running: bool,
    // 会话签名：由 "AppID + 启动时间" 组成
    // 客户端只需要判断这个字符串是否变化，不需要关心里面是什么
    pub signature: String,
}
