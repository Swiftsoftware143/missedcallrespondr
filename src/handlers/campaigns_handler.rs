use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::campaign::{Campaign, CreateCampaignRequest, UpdateCampaignRequest},
    state::AppState,
};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub status: Option<String>,
}

pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Campaign>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = if let Some(status) = &q.status {
        sqlx::query_as::<_, Campaign>(
            "SELECT * FROM campaigns WHERE tenant_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid)
        .bind(status)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, Campaign>(
            "SELECT * FROM campaigns WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
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
    Json(req): Json<CreateCampaignRequest>,
) -> Result<Json<Campaign>, AppError> {
    crate::features::enforce_feature_limit(&state.pool, claims.aid, "max_campaigns", "Campaigns")
        .await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO campaigns (id, tenant_id, name, kind, is_active, status, metadata, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.name)
    .bind(req.kind.as_deref().unwrap_or("manual"))
    .bind(req.is_active.unwrap_or(true))
    .bind(req.status.as_deref().unwrap_or("draft"))
    .bind(req.metadata.clone().unwrap_or(serde_json::json!({})))
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Campaign>("SELECT * FROM campaigns WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;

    // Each campaign owns its own fresh list, named after the campaign
    // (campaign "Inbound Plumbing" -> list "Inbound Plumbing").
    crate::handlers::lists_handler::ensure_campaign_list(&state, &claims.aid, &item.id, &item.name)
        .await;

    Ok(Json(item))
}

pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Campaign>, AppError> {
    let item =
        sqlx::query_as::<_, Campaign>("SELECT * FROM campaigns WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Campaign not found".into()))?;
    Ok(Json(item))
}

pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateCampaignRequest>,
) -> Result<Json<Campaign>, AppError> {
    let existing =
        sqlx::query_as::<_, Campaign>("SELECT * FROM campaigns WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Campaign not found".into()))?;
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE campaigns SET name=$1, kind=$2, is_active=$3, status=$4, metadata=$5, updated_at=$6 WHERE id=$7",
    )
    .bind(req.name.as_ref().unwrap_or(&existing.name))
    .bind(req.kind.as_ref().unwrap_or(&existing.kind))
    .bind(req.is_active.unwrap_or(existing.is_active))
    .bind(req.status.as_ref().unwrap_or(&existing.status))
    .bind(req.metadata.clone().unwrap_or(existing.metadata))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Campaign>("SELECT * FROM campaigns WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn activate(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("UPDATE campaigns SET status='active', is_active=true, updated_at=NOW() WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Campaign not found".into()));
    }
    Ok(Json(serde_json::json!({"id": id, "activated": true})))
}

pub async fn pause(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("UPDATE campaigns SET status='paused', is_active=false, updated_at=NOW() WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Campaign not found".into()));
    }
    Ok(Json(serde_json::json!({"id": id, "paused": true})))
}

pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM campaigns WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Campaign not found".into()));
    }
    Ok(Json(serde_json::json!({"deleted": true, "id": id})))
}
