use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct TenantSetting {
    pub tenant_id: Uuid,
    pub key: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateSettingsRequest {
    pub settings: Vec<SettingEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SettingEntry {
    pub key: String,
    pub value: serde_json::Value,
}
