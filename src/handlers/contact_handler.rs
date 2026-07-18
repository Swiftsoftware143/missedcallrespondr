use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use std::collections::HashMap;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::contact::{Contact, CreateContactRequest, UpdateContactRequest},
    state::AppState, features,
};

pub async fn list_contacts(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<Contact>>, AppError> {
    let items = sqlx::query_as::<_, Contact>(
        "SELECT * FROM contacts WHERE tenant_id = $1 ORDER BY name ASC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

pub async fn create_contact(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateContactRequest>,
) -> Result<Json<Contact>, AppError> {
    features::enforce_feature_limit(&state.pool, claims.aid, "max_contacts", "Contacts").await?;

    // Check for duplicate email within tenant
    if let Some(ref email) = req.email {
        if !email.trim().is_empty() {
            let existing: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM contacts WHERE email = $1 AND tenant_id = $2)",
            )
            .bind(email)
            .bind(claims.aid)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(false);

            if existing {
                return Err(AppError::BadRequest(format!(
                    "A contact with email '{}' already exists", email
                )));
            }
        }
    }

    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO contacts (id, name, phone, email, company, notes, tags, tenant_id, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.phone)
    .bind(&req.email)
    .bind(&req.company)
    .bind(&req.notes)
    .bind(&req.tags)
    .bind(claims.aid)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, Contact>("SELECT * FROM contacts WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;

    // Best-effort pushes (never block on failure)
    let state_clone = state.clone();
    let item_clone = item.clone();
    tokio::spawn(async move {
        super::workflowswift_push::push_contact_to_workflowswift(&state_clone, &item_clone).await;
        super::coreswift_push::push_contact_to_coreswift(&state_clone, &item_clone, "contact_creation").await;
    });

    Ok(Json(item))
}

pub async fn get_contact(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Contact>, AppError> {
    let item = sqlx::query_as::<_, Contact>(
        "SELECT * FROM contacts WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Contact not found".into()))?;
    Ok(Json(item))
}

pub async fn update_contact(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateContactRequest>,
) -> Result<Json<Contact>, AppError> {
    let existing = sqlx::query_as::<_, Contact>(
        "SELECT * FROM contacts WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Contact not found".into()))?;

    let now = chrono::Utc::now().naive_utc();
    sqlx::query(
        "UPDATE contacts SET name=$1, phone=$2, email=$3, company=$4, notes=$5, tags=$6, updated_at=$7 WHERE id=$8",
    )
    .bind(req.name.unwrap_or(existing.name))
    .bind(req.phone.unwrap_or(existing.phone))
    .bind(req.email.or(existing.email))
    .bind(req.company.or(existing.company))
    .bind(req.notes.or(existing.notes))
    .bind(req.tags.or(existing.tags))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, Contact>("SELECT * FROM contacts WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;

    // Best-effort push to CoreSwift (tag changes)
    let state_clone = state.clone();
    let item_clone = item.clone();
    tokio::spawn(async move {
        super::coreswift_push::push_contact_to_coreswift(&state_clone, &item_clone, "tag_update").await;
    });

    Ok(Json(item))
}

pub async fn delete_contact(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM contacts WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Contact not found".into()));
    }
    Ok(Json(serde_json::json!({"message": "Contact deleted"})))
}

pub async fn search_contacts(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<Contact>>, AppError> {
    let query = params.get("q").map(|s| format!("%{}%", s));
    let phone = params.get("phone");

    let items = if let Some(p) = phone {
        sqlx::query_as::<_, Contact>(
            "SELECT * FROM contacts WHERE tenant_id = $1 AND phone LIKE $2 ORDER BY name ASC",
        )
        .bind(claims.aid)
        .bind(p)
        .fetch_all(&state.pool)
        .await?
    } else if let Some(q) = query {
        sqlx::query_as::<_, Contact>(
            "SELECT * FROM contacts WHERE tenant_id = $1 AND (name ILIKE $2 OR phone ILIKE $2 OR email ILIKE $2) ORDER BY name ASC",
        )
        .bind(claims.aid)
        .bind(&q)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, Contact>(
            "SELECT * FROM contacts WHERE tenant_id = $1 ORDER BY name ASC",
        )
        .bind(claims.aid)
        .fetch_all(&state.pool)
        .await?
    };

    Ok(Json(items))
}
