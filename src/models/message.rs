use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct Message {
    pub id: Uuid,
    pub call_id: Option<Uuid>,
    pub direction: String,
    pub from_number: String,
    pub to_number: String,
    pub body: String,
    pub status: String,
    pub sent_at: Option<NaiveDateTime>,
    pub delivered_at: Option<NaiveDateTime>,
    pub tenant_id: Uuid,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateMessageRequest {
    pub call_id: Option<Uuid>,
    pub direction: String,
    pub from_number: String,
    pub to_number: String,
    pub body: String,
}
