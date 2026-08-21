use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct List {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub campaign_id: Option<Uuid>,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateListRequest {
    pub name: String,
    pub campaign_id: Option<Uuid>,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateListRequest {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// Request to add/remove a lead to/from a list.
#[derive(Debug, Serialize, Deserialize)]
pub struct ListLeadRequest {
    pub lead_id: Uuid,
}

/// A lead row inside a list (lightweight, for display in campaign lists).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ListLead {
    pub id: Uuid,
    pub name: String,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub status: String,
    pub tags: Option<Vec<String>>,
    pub added_at: Option<DateTime<Utc>>,
}
