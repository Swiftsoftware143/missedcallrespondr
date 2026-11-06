use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Ticket {
    pub id: Uuid,
    /// `None` for a message filed from the public CONTACT page (2026-11-06): that visitor is not
    /// signed in and belongs to no workspace, so the row carries no tenant and the address to answer
    /// travels in `requester_email` instead. Every ticket filed by a signed-in caller still names one.
    pub tenant_id: Option<Uuid>,
    pub subject: String,
    pub status: String,
    pub priority: String,
    pub assigned_to: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The address an operator's reply goes to when the message came from the public contact page
    /// (`tenant_id` is NULL). `None` on every ticket filed from inside a workspace, whose owner's
    /// address is looked up from `users` instead.
    pub requester_email: Option<String>,
    /// The tier a plan request asks for (cron 2026-10-20). `None` on an ordinary ticket.
    pub plan_slug: Option<String>,
    /// When an operator's resolve seated `plan_slug` on the workspace. `None` = not applied yet.
    pub plan_applied_at: Option<DateTime<Utc>>,
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
    /// Who is FILING this ticket. `create()` used to store the description as `agent`
    /// unconditionally, so a customer's own words were labelled "The team" in the thread view
    /// (2026-10-19). Optional and defaulting to `agent`, which keeps every existing caller and the
    /// admin console's "+ New Ticket" exactly as they were.
    #[serde(default)]
    pub sender_type: Option<String>,
    pub priority: Option<String>,
    pub assigned_to: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub source: Option<String>,
    /// The plan catalogue slug this request is for (cron 2026-10-20). Optional: an ordinary ticket
    /// names no plan, and every existing caller keeps working unchanged.
    #[serde(default)]
    pub plan_slug: Option<String>,
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
