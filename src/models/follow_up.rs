use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct FollowUp {
    pub id: Uuid,
    pub call_id: Uuid,
    pub follow_type: String,
    pub scheduled_at: NaiveDateTime,
    pub completed_at: Option<NaiveDateTime>,
    pub status: String,
    pub notes: Option<String>,
    pub assigned_to: Option<Uuid>,
    pub tenant_id: Uuid,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateFollowUpRequest {
    pub call_id: Uuid,
    pub follow_type: String,
    pub scheduled_at: NaiveDateTime,
    pub notes: Option<String>,
    pub assigned_to: Option<Uuid>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateFollowUpRequest {
    pub follow_type: Option<String>,
    pub scheduled_at: Option<NaiveDateTime>,
    pub completed_at: Option<NaiveDateTime>,
    pub status: Option<String>,
    pub notes: Option<String>,
    pub assigned_to: Option<Uuid>,
}
