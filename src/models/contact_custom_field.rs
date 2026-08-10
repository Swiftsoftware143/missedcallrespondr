use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ContactCustomField {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub field_name: String,
    pub field_type: String,
    pub is_required: bool,
    pub field_order: i32,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateCustomFieldRequest {
    pub field_name: String,
    pub field_type: Option<String>,
    pub is_required: Option<bool>,
    pub field_order: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateCustomFieldRequest {
    pub field_name: Option<String>,
    pub field_type: Option<String>,
    pub is_required: Option<bool>,
    pub field_order: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ContactFieldValue {
    pub id: Uuid,
    pub contact_id: Uuid,
    pub field_id: Uuid,
    pub value: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ContactWithFields {
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
    pub custom_fields: Vec<CustomFieldEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct CustomFieldEntry {
    #[sqlx(rename = "field_id")]
    pub field_id: Uuid,
    #[sqlx(rename = "field_name")]
    pub field_name: String,
    #[sqlx(rename = "field_type")]
    pub field_type: String,
    pub value: Option<String>,
}
