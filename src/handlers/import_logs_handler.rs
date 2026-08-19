use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::import_log::{CreateImportLogRequest, ImportLog},
    state::AppState,
};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub entity: Option<String>,
}

pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<ImportLog>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = if let Some(entity) = &q.entity {
        sqlx::query_as::<_, ImportLog>(
            "SELECT * FROM import_logs WHERE tenant_id = $1 AND entity = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid).bind(entity).bind(limit).bind(offset).fetch_all(&state.pool).await?
    } else {
        sqlx::query_as::<_, ImportLog>(
            "SELECT * FROM import_logs WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(claims.aid).bind(limit).bind(offset).fetch_all(&state.pool).await?
    };
    Ok(Json(items))
}

pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateImportLogRequest>,
) -> Result<Json<ImportLog>, AppError> {
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO import_logs (id, tenant_id, entity, filename, status, total_rows, inserted, failed, error_summary, created_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(req.entity.as_deref().unwrap_or("contacts"))
    .bind(req.filename.as_deref().unwrap_or(""))
    .bind(req.status.as_deref().unwrap_or("pending"))
    .bind(req.total_rows.unwrap_or(0))
    .bind(req.inserted.unwrap_or(0))
    .bind(req.failed.unwrap_or(0))
    .bind(&req.error_summary)
    .bind(now)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, ImportLog>("SELECT * FROM import_logs WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ImportLog>, AppError> {
    let item = sqlx::query_as::<_, ImportLog>(
        "SELECT * FROM import_logs WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Import log not found".into()))?;
    Ok(Json(item))
}

pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateImportLogRequest>,
) -> Result<Json<ImportLog>, AppError> {
    let existing = sqlx::query_as::<_, ImportLog>(
        "SELECT * FROM import_logs WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Import log not found".into()))?;
    sqlx::query(
        "UPDATE import_logs SET entity=$1, filename=$2, status=$3, total_rows=$4, inserted=$5, failed=$6, error_summary=$7 WHERE id=$8",
    )
    .bind(req.entity.as_ref().unwrap_or(&existing.entity))
    .bind(req.filename.as_ref().unwrap_or(&existing.filename))
    .bind(req.status.as_ref().unwrap_or(&existing.status))
    .bind(req.total_rows.unwrap_or(existing.total_rows))
    .bind(req.inserted.unwrap_or(existing.inserted))
    .bind(req.failed.unwrap_or(existing.failed))
    .bind(req.error_summary.as_ref().or(existing.error_summary.as_ref()))
    .bind(id)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, ImportLog>("SELECT * FROM import_logs WHERE id = $1")
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
    let result = sqlx::query("DELETE FROM import_logs WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Import log not found".into()));
    }
    Ok(Json(serde_json::json!({"deleted": true, "id": id})))
}
