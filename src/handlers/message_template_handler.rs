use axum::{
    extract::{Extension, Path, State},
    Json,
};
use uuid::Uuid;

use crate::features;
use crate::{
    config::Claims,
    error::AppError,
    models::message_template::{
        CreateMessageTemplateRequest, MessageTemplate, UpdateMessageTemplateRequest,
    },
    state::AppState,
};

pub async fn list_message_templates(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<MessageTemplate>>, AppError> {
    let items = sqlx::query_as::<_, MessageTemplate>(
        "SELECT * FROM message_templates WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

pub async fn create_message_template(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateMessageTemplateRequest>,
) -> Result<Json<MessageTemplate>, AppError> {
    let tenant_id: Uuid = claims.aid;
    features::enforce_feature_limit(&state.pool, tenant_id, "max_message_templates", "Message Templates").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO message_templates (id, name, body, variables, tenant_id, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.body)
    .bind(&req.variables)
    .bind(claims.aid)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, MessageTemplate>("SELECT * FROM message_templates WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn update_message_template(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateMessageTemplateRequest>,
) -> Result<Json<MessageTemplate>, AppError> {
    let existing = sqlx::query_as::<_, MessageTemplate>(
        "SELECT * FROM message_templates WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Message template not found".into()))?;

    let now = chrono::Utc::now().naive_utc();
    sqlx::query(
        "UPDATE message_templates SET name=$1, body=$2, variables=$3, updated_at=$4 WHERE id=$5",
    )
    .bind(req.name.unwrap_or(existing.name))
    .bind(req.body.unwrap_or(existing.body))
    .bind(req.variables.or(existing.variables))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, MessageTemplate>("SELECT * FROM message_templates WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn delete_message_template(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM message_templates WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Message template not found".into()));
    }
    Ok(Json(serde_json::json!({"message": "Message template deleted"})))
}
