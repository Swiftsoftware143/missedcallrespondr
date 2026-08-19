use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ExportTemplate {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub entity: String,
    pub format: String,
    pub columns: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateExportTemplateRequest {
    pub name: String,
    pub entity: Option<String>,
    pub format: Option<String>,
    pub columns: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateExportTemplateRequest {
    pub name: Option<String>,
    pub entity: Option<String>,
    pub format: Option<String>,
    pub columns: Option<serde_json::Value>,
}
