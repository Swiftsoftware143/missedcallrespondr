use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Ticket {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub subject: String,
    pub status: String,
    pub priority: String,
    pub assigned_to: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct TicketMessage {
    pub id: Uuid,
    pub ticket_id: Uuid,
    pub sender_type: String,
    pub sender_id: Option<Uuid>,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateTicketRequest {
    pub subject: String,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub assigned_to: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub source: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateTicketRequest {
    pub subject: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub assigned_to: Option<Uuid>,
    pub contact_id: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateTicketMessageRequest {
    pub sender_type: Option<String>,
    pub sender_id: Option<Uuid>,
    pub body: String,
}
