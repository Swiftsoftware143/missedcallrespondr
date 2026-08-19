use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Deal {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub contact_id: Option<Uuid>,
    pub lead_id: Option<Uuid>,
    pub value: Decimal,
    pub stage: String,
    pub probability: i32,
    pub expected_close_date: Option<DateTime<Utc>>,
    pub is_won: bool,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateDealRequest {
    pub name: String,
    pub contact_id: Option<Uuid>,
    pub lead_id: Option<Uuid>,
    pub value: Option<Decimal>,
    pub stage: Option<String>,
    pub probability: Option<i32>,
    pub expected_close_date: Option<DateTime<Utc>>,
    pub is_won: Option<bool>,
    pub source: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateDealRequest {
    pub name: Option<String>,
    pub contact_id: Option<Uuid>,
    pub lead_id: Option<Uuid>,
    pub value: Option<Decimal>,
    pub stage: Option<String>,
    pub probability: Option<i32>,
    pub expected_close_date: Option<DateTime<Utc>>,
    pub is_won: Option<bool>,
    pub source: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MoveDealRequest {
    pub stage: String,
}
