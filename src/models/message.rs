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
    /// The PROVIDER'S OWN message id (kanban t_2ed95642) — the key the Telnyx message events
    /// (`message.sent` / `message.finalized`) are matched by. NULL means "no provider reference":
    /// a recorded inbound message, or a row written before the transport existed.
    pub provider_message_id: Option<String>,
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
