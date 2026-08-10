use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct MessageTemplate {
    pub id: Uuid,
    pub name: String,
    pub body: String,
    pub variables: Option<Vec<String>>,
    pub tenant_id: Uuid,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateMessageTemplateRequest {
    pub name: String,
    pub body: String,
    pub variables: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateMessageTemplateRequest {
    pub name: Option<String>,
    pub body: Option<String>,
    pub variables: Option<Vec<String>>,
}
