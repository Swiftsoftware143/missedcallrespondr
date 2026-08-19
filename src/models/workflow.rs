use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Workflow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub trigger_event: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WorkflowStep {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub step_order: i32,
    pub action_type: String,
    pub action_config: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateWorkflowRequest {
    pub name: String,
    pub trigger_event: Option<String>,
    pub is_active: Option<bool>,
    pub steps: Option<Vec<CreateWorkflowStepRequest>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateWorkflowStepRequest {
    pub step_order: i32,
    pub action_type: String,
    pub action_config: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateWorkflowRequest {
    pub name: Option<String>,
    pub trigger_event: Option<String>,
    pub is_active: Option<bool>,
}
