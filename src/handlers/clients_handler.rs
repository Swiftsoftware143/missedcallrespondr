use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::client::{Client, CreateClientRequest, UpdateClientRequest},
    state::AppState,
};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub search: Option<String>,
}

pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Client>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = if let Some(search) = &q.search {
        let pat = format!("%{}%", search);
        sqlx::query_as::<_, Client>(
            "SELECT * FROM clients WHERE tenant_id = $1 AND (name ILIKE $2 OR email ILIKE $2 OR phone ILIKE $2) ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid)
        .bind(&pat)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, Client>(
            "SELECT * FROM clients WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(claims.aid)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    };
    Ok(Json(items))
}

pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateClientRequest>,
) -> Result<Json<Client>, AppError> {
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO clients (id, tenant_id, name, email, phone, source, notes, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.name)
    .bind(&req.email)
    .bind(&req.phone)
    .bind(req.source.as_deref().unwrap_or("manual"))
    .bind(&req.notes)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Client>("SELECT * FROM clients WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Client>, AppError> {
    let item =
        sqlx::query_as::<_, Client>("SELECT * FROM clients WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Client not found".into()))?;
    Ok(Json(item))
}

pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateClientRequest>,
) -> Result<Json<Client>, AppError> {
    let existing =
        sqlx::query_as::<_, Client>("SELECT * FROM clients WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Client not found".into()))?;
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE clients SET name=$1, email=$2, phone=$3, source=$4, notes=$5, updated_at=$6 WHERE id=$7",
    )
    .bind(req.name.as_ref().unwrap_or(&existing.name))
    .bind(req.email.as_ref().or(existing.email.as_ref()))
    .bind(req.phone.as_ref().or(existing.phone.as_ref()))
    .bind(req.source.as_ref().unwrap_or(&existing.source))
    .bind(req.notes.as_ref().or(existing.notes.as_ref()))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Client>("SELECT * FROM clients WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM clients WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Client not found".into()));
    }
    Ok(Json(serde_json::json!({"deleted": true, "id": id})))
}
