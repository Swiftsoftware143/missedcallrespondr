use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, FromRow)]
#[allow(dead_code)]
pub struct DashboardStat {
    pub tenant_id: Uuid,
    pub period: String,
    pub total_calls: i64,
    pub missed_calls: i64,
    pub answered_calls: i64,
    pub response_rate: Option<f64>,
    pub avg_response_time: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DashboardResponse {
    pub total_calls: i64,
    pub missed_calls: i64,
    pub answered_calls: i64,
    pub voicemails: i64,
    pub follow_ups_pending: i64,
    pub response_rate: f64,
}
