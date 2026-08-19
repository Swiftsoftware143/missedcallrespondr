use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::workflow::{CreateWorkflowRequest, UpdateWorkflowRequest, Workflow, WorkflowStep},
    state::AppState,
};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Workflow>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = sqlx::query_as::<_, Workflow>(
        "SELECT * FROM workflows WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(claims.aid)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateWorkflowRequest>,
) -> Result<Json<Workflow>, AppError> {
    crate::features::enforce_feature_limit(&state.pool, claims.aid, "max_workflows", "Workflows")
        .await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO workflows (id, tenant_id, name, trigger_event, is_active, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.name)
    .bind(req.trigger_event.as_deref().unwrap_or("missed_call"))
    .bind(req.is_active.unwrap_or(true))
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    // Insert steps
    if let Some(steps) = &req.steps {
        for s in steps {
            let sid = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO workflow_steps (id, workflow_id, step_order, action_type, action_config, created_at) \
                 VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(sid)
            .bind(id)
            .bind(s.step_order)
            .bind(&s.action_type)
            .bind(s.action_config.clone().unwrap_or(serde_json::json!({})))
            .bind(now)
            .execute(&state.pool)
            .await?;
        }
    }

    let item = sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let wf =
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Workflow not found".into()))?;
    let steps = sqlx::query_as::<_, WorkflowStep>(
        "SELECT * FROM workflow_steps WHERE workflow_id = $1 ORDER BY step_order ASC",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(serde_json::json!({ "workflow": wf, "steps": steps })))
}

pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateWorkflowRequest>,
) -> Result<Json<Workflow>, AppError> {
    let existing =
        sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Workflow not found".into()))?;
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE workflows SET name=$1, trigger_event=$2, is_active=$3, updated_at=$4 WHERE id=$5",
    )
    .bind(req.name.as_ref().unwrap_or(&existing.name))
    .bind(
        req.trigger_event
            .as_ref()
            .unwrap_or(&existing.trigger_event),
    )
    .bind(req.is_active.unwrap_or(existing.is_active))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Workflow>("SELECT * FROM workflows WHERE id = $1")
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
    let result = sqlx::query("UPDATE workflows SET is_active = true, updated_at = NOW() WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Workflow not found".into()));
    }
    Ok(Json(serde_json::json!({"id": id, "activated": true})))
}

pub async fn deactivate(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("UPDATE workflows SET is_active = false, updated_at = NOW() WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Workflow not found".into()));
    }
    Ok(Json(serde_json::json!({"id": id, "deactivated": true})))
}

pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM workflows WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Workflow not found".into()));
    }
    Ok(Json(serde_json::json!({"deleted": true, "id": id})))
}
