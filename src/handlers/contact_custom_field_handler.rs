use axum::{
    extract::{Extension, Path, State},
    Json,
};
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::contact_custom_field::{
        ContactCustomField, CreateCustomFieldRequest, UpdateCustomFieldRequest,
        ContactWithFields, CustomFieldEntry,
    },
    state::AppState,
};

// --- Custom Field Definitions (per tenant) ---

pub async fn list_custom_fields(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ContactCustomField>>, AppError> {
    let fields = sqlx::query_as::<_, ContactCustomField>(
        "SELECT * FROM contact_custom_fields WHERE tenant_id = $1 ORDER BY field_order ASC, field_name ASC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(fields))
}

pub async fn create_custom_field(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateCustomFieldRequest>,
) -> Result<Json<ContactCustomField>, AppError> {
    if req.field_name.trim().is_empty() {
        return Err(AppError::BadRequest("field_name is required".into()));
    }

    // Check for duplicate name within tenant
    let existing: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM contact_custom_fields WHERE tenant_id = $1 AND field_name = $2)",
    )
    .bind(claims.aid)
    .bind(&req.field_name)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(false);

    if existing {
        return Err(AppError::BadRequest(format!(
            "A custom field named '{}' already exists", req.field_name
        )));
    }

    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();
    let field_type = req.field_type.unwrap_or_else(|| "text".into());
    let is_required = req.is_required.unwrap_or(false);
    let field_order = req.field_order.unwrap_or(0);

    sqlx::query(
        "INSERT INTO contact_custom_fields (id, tenant_id, field_name, field_type, is_required, field_order, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.field_name)
    .bind(&field_type)
    .bind(is_required)
    .bind(field_order)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let field = sqlx::query_as::<_, ContactCustomField>("SELECT * FROM contact_custom_fields WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(field))
}

pub async fn update_custom_field(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateCustomFieldRequest>,
) -> Result<Json<ContactCustomField>, AppError> {
    let existing = sqlx::query_as::<_, ContactCustomField>(
        "SELECT * FROM contact_custom_fields WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Custom field not found".into()))?;

    sqlx::query(
        "UPDATE contact_custom_fields SET field_name=$1, field_type=$2, is_required=$3, field_order=$4 WHERE id=$5",
    )
    .bind(req.field_name.unwrap_or(existing.field_name))
    .bind(req.field_type.unwrap_or(existing.field_type))
    .bind(req.is_required.unwrap_or(existing.is_required))
    .bind(req.field_order.unwrap_or(existing.field_order))
    .bind(id)
    .execute(&state.pool)
    .await?;

    let field = sqlx::query_as::<_, ContactCustomField>("SELECT * FROM contact_custom_fields WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(field))
}

pub async fn delete_custom_field(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM contact_custom_fields WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Custom field not found".into()));
    }
    Ok(Json(serde_json::json!({"message": "Custom field deleted"})))
}

// --- Contact + Custom Fields combined ---

pub async fn get_contact_with_fields(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ContactWithFields>, AppError> {
    let contact = sqlx::query_as::<_, crate::models::contact::Contact>(
        "SELECT * FROM contacts WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Contact not found".into()))?;

    let fields = sqlx::query_as::<_, CustomFieldEntry>(
        "SELECT cfv.field_id, ccf.field_name, ccf.field_type, cfv.value \
         FROM contact_field_values cfv \
         JOIN contact_custom_fields ccf ON ccf.id = cfv.field_id \
         WHERE cfv.contact_id = $1 \
         ORDER BY ccf.field_order ASC, ccf.field_name ASC",
    )
    .bind(contact.id)
    .fetch_all(&state.pool)
    .await?;

    let custom_fields: Vec<CustomFieldEntry> = fields.into_iter().map(|f| CustomFieldEntry {
        field_id: f.field_id,
        field_name: f.field_name,
        field_type: f.field_type,
        value: f.value,
    }).collect();

    Ok(Json(ContactWithFields {
        id: contact.id,
        name: contact.name,
        phone: contact.phone,
        email: contact.email,
        company: contact.company,
        notes: contact.notes,
        tags: contact.tags,
        tenant_id: contact.tenant_id,
        created_at: contact.created_at,
        updated_at: contact.updated_at,
        custom_fields,
    }))
}

pub async fn list_contacts_with_fields(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ContactWithFields>>, AppError> {
    let contacts = sqlx::query_as::<_, crate::models::contact::Contact>(
        "SELECT * FROM contacts WHERE tenant_id = $1 ORDER BY name ASC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;

    let mut result: Vec<ContactWithFields> = Vec::new();
    for contact in contacts {
        let fields = sqlx::query_as::<_, CustomFieldEntry>(
            "SELECT cfv.field_id, ccf.field_name, ccf.field_type, cfv.value \
             FROM contact_field_values cfv \
             JOIN contact_custom_fields ccf ON ccf.id = cfv.field_id \
             WHERE cfv.contact_id = $1 \
             ORDER BY ccf.field_order ASC, ccf.field_name ASC",
        )
        .bind(contact.id)
        .fetch_all(&state.pool)
        .await
        .unwrap_or_default();

        let custom_fields: Vec<CustomFieldEntry> = fields.into_iter().map(|f| CustomFieldEntry {
            field_id: f.field_id,
            field_name: f.field_name,
            field_type: f.field_type,
            value: f.value,
        }).collect();

        result.push(ContactWithFields {
            id: contact.id,
            name: contact.name,
            phone: contact.phone,
            email: contact.email,
            company: contact.company,
            notes: contact.notes,
            tags: contact.tags,
            tenant_id: contact.tenant_id,
            created_at: contact.created_at,
            updated_at: contact.updated_at,
            custom_fields,
        });
    }

    Ok(Json(result))
}

pub async fn update_contact_field_value(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path((contact_id, field_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<crate::models::contact_custom_field::ContactFieldValue>, AppError> {
    let value = req.get("value").and_then(|v| v.as_str()).unwrap_or("");

    // Verify contact belongs to tenant
    let contact_ok: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM contacts WHERE id = $1 AND tenant_id = $2)",
    )
    .bind(contact_id)
    .bind(claims.aid)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(false);

    if !contact_ok {
        return Err(AppError::NotFound("Contact not found".into()));
    }

    // Upsert field value
    sqlx::query(
        "INSERT INTO contact_field_values (contact_id, field_id, value, updated_at) \
         VALUES ($1, $2, $3, NOW()) \
         ON CONFLICT (contact_id, field_id) \
         DO UPDATE SET value = $3, updated_at = NOW()",
    )
    .bind(contact_id)
    .bind(field_id)
    .bind(value)
    .execute(&state.pool)
    .await?;

    let entry = sqlx::query_as::<_, crate::models::contact_custom_field::ContactFieldValue>(
        "SELECT * FROM contact_field_values WHERE contact_id = $1 AND field_id = $2",
    )
    .bind(contact_id)
    .bind(field_id)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(entry))
}
