use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ImportLog {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub entity: String,
    pub filename: String,
    pub status: String,
    pub total_rows: i32,
    pub inserted: i32,
    pub failed: i32,
    pub error_summary: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateImportLogRequest {
    pub entity: Option<String>,
    pub filename: Option<String>,
    pub status: Option<String>,
    pub total_rows: Option<i32>,
    pub inserted: Option<i32>,
    pub failed: Option<i32>,
    pub error_summary: Option<String>,
}
