use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A response rule: what the service does automatically when a call comes in.
///
/// NOTE (kanban t_31f9cf38) — this store is no longer CRUD-only. `crate::handlers::response_rule_eval`
/// reads it on every inbound call: ACTIVE rules for the called tenant are walked in `priority` order
/// (lowest number first, `created_at` breaking ties) and the FIRST rule whose `trigger_condition`
/// matches fires, then evaluation stops. See that module for the trigger vocabulary and for what each
/// `response_type` actually does.
#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct ResponseRule {
    pub id: Uuid,
    pub name: String,
    pub trigger_condition: String,
    pub response_type: String,
    pub response_content: serde_json::Value,
    pub schedule: Option<serde_json::Value>,
    pub tenant_id: Uuid,
    pub is_active: bool,
    /// Lower fires first; 100 is the documented "no preference" position.
    pub priority: i32,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateResponseRuleRequest {
    pub name: String,
    pub trigger_condition: String,
    pub response_type: String,
    pub response_content: serde_json::Value,
    pub schedule: Option<serde_json::Value>,
    pub is_active: Option<bool>,
    pub priority: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateResponseRuleRequest {
    pub name: Option<String>,
    pub trigger_condition: Option<String>,
    pub response_type: Option<String>,
    pub response_content: Option<serde_json::Value>,
    pub schedule: Option<serde_json::Value>,
    pub is_active: Option<bool>,
    pub priority: Option<i32>,
}
