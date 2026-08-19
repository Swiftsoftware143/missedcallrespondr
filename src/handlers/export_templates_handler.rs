use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::export_template::{
        CreateExportTemplateRequest, ExportTemplate, UpdateExportTemplateRequest,
    },
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
) -> Result<Json<Vec<ExportTemplate>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = if let Some(entity) = &q.entity {
        sqlx::query_as::<_, ExportTemplate>(
            "SELECT * FROM export_templates WHERE tenant_id = $1 AND entity = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid).bind(entity).bind(limit).bind(offset).fetch_all(&state.pool).await?
    } else {
        sqlx::query_as::<_, ExportTemplate>(
            "SELECT * FROM export_templates WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(claims.aid).bind(limit).bind(offset).fetch_all(&state.pool).await?
    };
    Ok(Json(items))
}

pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateExportTemplateRequest>,
) -> Result<Json<ExportTemplate>, AppError> {
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO export_templates (id, tenant_id, name, entity, format, columns, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.name)
    .bind(req.entity.as_deref().unwrap_or("contacts"))
    .bind(req.format.as_deref().unwrap_or("csv"))
    .bind(req.columns.clone().unwrap_or(serde_json::json!([])))
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, ExportTemplate>("SELECT * FROM export_templates WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ExportTemplate>, AppError> {
    let item = sqlx::query_as::<_, ExportTemplate>(
        "SELECT * FROM export_templates WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Export template not found".into()))?;
    Ok(Json(item))
}

pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateExportTemplateRequest>,
) -> Result<Json<ExportTemplate>, AppError> {
    let existing = sqlx::query_as::<_, ExportTemplate>(
        "SELECT * FROM export_templates WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Export template not found".into()))?;
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE export_templates SET name=$1, entity=$2, format=$3, columns=$4, updated_at=$5 WHERE id=$6",
    )
    .bind(req.name.as_ref().unwrap_or(&existing.name))
    .bind(req.entity.as_ref().unwrap_or(&existing.entity))
    .bind(req.format.as_ref().unwrap_or(&existing.format))
    .bind(req.columns.clone().unwrap_or(existing.columns))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, ExportTemplate>("SELECT * FROM export_templates WHERE id = $1")
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
    let result = sqlx::query("DELETE FROM export_templates WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Export template not found".into()));
    }
    Ok(Json(serde_json::json!({"deleted": true, "id": id})))
}
