use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct InboundCall {
    pub id: Uuid,
    pub caller_number: String,
    pub caller_name: Option<String>,
    pub called_number: String,
    pub call_time: NaiveDateTime,
    pub duration: Option<i32>,
    pub recording_url: Option<String>,
    pub voicemail_url: Option<String>,
    pub disposition: String,
    pub tenant_id: Uuid,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateInboundCallRequest {
    pub caller_number: String,
    pub caller_name: Option<String>,
    pub called_number: String,
    pub call_time: Option<NaiveDateTime>,
    pub duration: Option<i32>,
    pub recording_url: Option<String>,
    pub voicemail_url: Option<String>,
    pub disposition: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateInboundCallRequest {
    pub caller_name: Option<String>,
    pub duration: Option<i32>,
    pub recording_url: Option<String>,
    pub voicemail_url: Option<String>,
    pub disposition: Option<String>,
}
