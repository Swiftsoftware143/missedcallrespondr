use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::lead::{CreateLeadRequest, Lead, UpdateLeadRequest},
    state::AppState,
};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub search: Option<String>,
    pub status: Option<String>,
}

pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Lead>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = if let (Some(status), Some(search)) = (&q.status, &q.search) {
        let pat = format!("%{}%", search);
        sqlx::query_as::<_, Lead>(
            "SELECT * FROM leads WHERE tenant_id = $1 AND status = $2 AND (name ILIKE $3 OR phone ILIKE $3 OR email ILIKE $3) ORDER BY created_at DESC LIMIT $4 OFFSET $5",
        )
        .bind(claims.aid)
        .bind(status)
        .bind(&pat)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else if let Some(status) = &q.status {
        sqlx::query_as::<_, Lead>(
            "SELECT * FROM leads WHERE tenant_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid)
        .bind(status)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else if let Some(search) = &q.search {
        let pat = format!("%{}%", search);
        sqlx::query_as::<_, Lead>(
            "SELECT * FROM leads WHERE tenant_id = $1 AND (name ILIKE $2 OR phone ILIKE $2 OR email ILIKE $2) ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid)
        .bind(&pat)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, Lead>(
            "SELECT * FROM leads WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
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
    Json(req): Json<CreateLeadRequest>,
) -> Result<Json<Lead>, AppError> {
    crate::features::enforce_feature_limit(&state.pool, claims.aid, "max_leads", "Leads").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO leads (id, tenant_id, name, phone, email, source, status, notes, tags, call_id, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.name)
    .bind(&req.phone)
    .bind(&req.email)
    .bind(req.source.as_deref().unwrap_or("call"))
    .bind(req.status.as_deref().unwrap_or("new"))
    .bind(&req.notes)
    .bind(&req.tags)
    .bind(req.call_id)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Lead>("SELECT * FROM leads WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Lead>, AppError> {
    let item = sqlx::query_as::<_, Lead>("SELECT * FROM leads WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Lead not found".into()))?;
    Ok(Json(item))
}

pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateLeadRequest>,
) -> Result<Json<Lead>, AppError> {
    let existing =
        sqlx::query_as::<_, Lead>("SELECT * FROM leads WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Lead not found".into()))?;
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE leads SET name=$1, phone=$2, email=$3, source=$4, status=$5, notes=$6, tags=$7, call_id=$8, updated_at=$9 WHERE id=$10",
    )
    .bind(req.name.as_ref().unwrap_or(&existing.name))
    .bind(req.phone.as_ref().or(existing.phone.as_ref()))
    .bind(req.email.as_ref().or(existing.email.as_ref()))
    .bind(req.source.as_ref().unwrap_or(&existing.source))
    .bind(req.status.as_ref().unwrap_or(&existing.status))
    .bind(req.notes.as_ref().or(existing.notes.as_ref()))
    .bind(req.tags.as_ref().or(existing.tags.as_ref()))
    .bind(req.call_id.or(existing.call_id))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Lead>("SELECT * FROM leads WHERE id = $1")
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
    let result = sqlx::query("DELETE FROM leads WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Lead not found".into()));
    }
    Ok(Json(serde_json::json!({"deleted": true, "id": id})))
}
