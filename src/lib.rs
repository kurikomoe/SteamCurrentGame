use serde::{Deserialize, Serialize};


#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct ReportData {
    pub app_id: u32,
    pub token: String,
}
