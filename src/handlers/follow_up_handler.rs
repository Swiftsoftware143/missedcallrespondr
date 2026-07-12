use axum::{
    extract::{Extension, Path, State},
    Json,
};
use uuid::Uuid;

use crate::features;
use crate::{
    config::Claims,
    error::AppError,
    models::follow_up::{CreateFollowUpRequest, FollowUp, UpdateFollowUpRequest},
    state::AppState,
};

pub async fn list_follow_ups(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<FollowUp>>, AppError> {
    let items = sqlx::query_as::<_, FollowUp>(
        "SELECT * FROM follow_ups WHERE tenant_id = $1 ORDER BY scheduled_at DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

pub async fn create_follow_up(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateFollowUpRequest>,
) -> Result<Json<FollowUp>, AppError> {
    let tenant_id: Uuid = claims.aid;
    features::enforce_feature_limit(&state.pool, tenant_id, "max_follow_ups", "Follow Ups").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO follow_ups (id, call_id, follow_type, scheduled_at, status, notes, assigned_to, tenant_id, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(req.call_id)
    .bind(&req.follow_type)
    .bind(req.scheduled_at)
    .bind("pending")
    .bind(&req.notes)
    .bind(req.assigned_to)
    .bind(claims.aid)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, FollowUp>("SELECT * FROM follow_ups WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn update_follow_up(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateFollowUpRequest>,
) -> Result<Json<FollowUp>, AppError> {
    let existing = sqlx::query_as::<_, FollowUp>(
        "SELECT * FROM follow_ups WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Follow-up not found".into()))?;

    let now = chrono::Utc::now().naive_utc();
    sqlx::query(
        "UPDATE follow_ups SET follow_type=$1, scheduled_at=$2, completed_at=$3, status=$4, notes=$5, assigned_to=$6, updated_at=$7 WHERE id=$8",
    )
    .bind(req.follow_type.unwrap_or(existing.follow_type))
    .bind(req.scheduled_at.unwrap_or(existing.scheduled_at))
    .bind(req.completed_at.or(existing.completed_at))
    .bind(req.status.unwrap_or(existing.status))
    .bind(req.notes.or(existing.notes))
    .bind(req.assigned_to.or(existing.assigned_to))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, FollowUp>("SELECT * FROM follow_ups WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn delete_follow_up(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM follow_ups WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Follow-up not found".into()));
    }
    Ok(Json(serde_json::json!({"message": "Follow-up deleted"})))
}
