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
    pub follow_ups_pending: i64,
    pub response_rate: f64,
    // Counts the panel's tiles read. Added 2026-10-18: the console rendered the four "..." tiles
    // from these fields and the handler never sent them, so every tile stayed on "..." for ever.
    pub total_rules: i64,
    pub total_templates: i64,
    pub total_contacts: i64,
    pub total_responses: i64,
}
