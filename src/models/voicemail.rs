use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct Voicemail {
    pub id: Uuid,
    pub call_id: Uuid,
    pub audio_url: Option<String>,
    pub transcription: Option<String>,
    pub duration: Option<i32>,
    pub listened: bool,
    pub notes: Option<String>,
    pub tenant_id: Uuid,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateVoicemailRequest {
    pub listened: Option<bool>,
    pub notes: Option<String>,
    pub transcription: Option<String>,
}
