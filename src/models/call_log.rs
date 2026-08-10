use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct CallLog {
    pub id: Uuid,
    pub caller_number: String,
    pub called_number: String,
    pub duration: Option<i32>,
    pub disposition: String,
    pub cost: Option<f64>,
    pub recorded: bool,
    pub notes: Option<String>,
    pub tenant_id: Uuid,
    pub created_at: NaiveDateTime,
}
