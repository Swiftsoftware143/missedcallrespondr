use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct ResponseRule {
    pub id: Uuid,
    pub name: String,
    pub trigger_condition: String,
    pub response_type: String,
    pub response_content: serde_json::Value,
    pub schedule: Option<serde_json::Value>,
    pub tenant_id: Uuid,
    pub is_active: bool,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateResponseRuleRequest {
    pub name: String,
    pub trigger_condition: String,
    pub response_type: String,
    pub response_content: serde_json::Value,
    pub schedule: Option<serde_json::Value>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateResponseRuleRequest {
    pub name: Option<String>,
    pub trigger_condition: Option<String>,
    pub response_type: Option<String>,
    pub response_content: Option<serde_json::Value>,
    pub schedule: Option<serde_json::Value>,
    pub is_active: Option<bool>,
}
