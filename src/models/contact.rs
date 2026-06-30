use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Contact {
    pub id: Uuid,
    pub name: String,
    pub phone: String,
    pub email: Option<String>,
    pub company: Option<String>,
    pub notes: Option<String>,
    pub tags: Option<Vec<String>>,
    pub tenant_id: Uuid,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateContactRequest {
    pub name: String,
    pub phone: String,
    pub email: Option<String>,
    pub company: Option<String>,
    pub notes: Option<String>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateContactRequest {
    pub name: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub company: Option<String>,
    pub notes: Option<String>,
    pub tags: Option<Vec<String>>,
}
