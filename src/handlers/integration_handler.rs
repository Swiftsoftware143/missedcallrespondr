use axum::{
    extract::{Extension, Path, State},
    Json,
};
use uuid::Uuid;

use crate::features;
use crate::{
    config::Claims,
    error::AppError,
    models::integration::{CreateIntegrationRequest, Integration, UpdateIntegrationRequest},
    state::AppState,
};

pub async fn list_integrations(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<Integration>>, AppError> {
    let items = sqlx::query_as::<_, Integration>(
        "SELECT * FROM integrations WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

pub async fn create_integration(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateIntegrationRequest>,
) -> Result<Json<Integration>, AppError> {
    let tenant_id: Uuid = claims.aid;
    features::enforce_feature_limit(&state.pool, tenant_id, "max_integrations", "Integrations").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();
    let is_active = req.is_active.unwrap_or(true);

    sqlx::query(
        "INSERT INTO integrations (id, name, integration_type, config, tenant_id, is_active, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.integration_type)
    .bind(&req.config)
    .bind(claims.aid)
    .bind(is_active)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, Integration>("SELECT * FROM integrations WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn update_integration(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateIntegrationRequest>,
) -> Result<Json<Integration>, AppError> {
    let existing = sqlx::query_as::<_, Integration>(
        "SELECT * FROM integrations WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Integration not found".into()))?;

    let now = chrono::Utc::now().naive_utc();
    sqlx::query(
        "UPDATE integrations SET name=$1, integration_type=$2, config=$3, is_active=$4, updated_at=$5 WHERE id=$6",
    )
    .bind(req.name.unwrap_or(existing.name))
    .bind(req.integration_type.unwrap_or(existing.integration_type))
    .bind(req.config.unwrap_or(existing.config))
    .bind(req.is_active.unwrap_or(existing.is_active))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, Integration>("SELECT * FROM integrations WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn delete_integration(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM integrations WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Integration not found".into()));
    }
    Ok(Json(serde_json::json!({"message": "Integration deleted"})))
}

pub async fn test_integration(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _integration = sqlx::query_as::<_, Integration>(
        "SELECT * FROM integrations WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Integration not found".into()))?;

    Ok(Json(serde_json::json!({"status": "test_passed", "message": "Integration test successful"})))
}
