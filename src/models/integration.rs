use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct Integration {
    pub id: Uuid,
    pub name: String,
    pub integration_type: String,
    pub config: serde_json::Value,
    pub tenant_id: Uuid,
    pub is_active: bool,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateIntegrationRequest {
    pub name: String,
    pub integration_type: String,
    pub config: serde_json::Value,
    pub is_active: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateIntegrationRequest {
    pub name: Option<String>,
    pub integration_type: Option<String>,
    pub config: Option<serde_json::Value>,
    pub is_active: Option<bool>,
}
