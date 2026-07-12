use axum::{
    extract::{Extension, Path, State},
    Json,
};
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::response_rule::{CreateResponseRuleRequest, ResponseRule, UpdateResponseRuleRequest},
    state::AppState, features,
};

pub async fn list_response_rules(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ResponseRule>>, AppError> {
    let rules = sqlx::query_as::<_, ResponseRule>(
        "SELECT * FROM response_rules WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rules))
}

pub async fn create_response_rule(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateResponseRuleRequest>,
) -> Result<Json<ResponseRule>, AppError> {
    features::enforce_feature_limit(&state.pool, claims.aid, "max_rules", "Response rules").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();
    let is_active = req.is_active.unwrap_or(true);

    sqlx::query(
        "INSERT INTO response_rules (id, name, trigger_condition, response_type, response_content, schedule, tenant_id, is_active, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.trigger_condition)
    .bind(&req.response_type)
    .bind(&req.response_content)
    .bind(&req.schedule)
    .bind(claims.aid)
    .bind(is_active)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let rule = sqlx::query_as::<_, ResponseRule>("SELECT * FROM response_rules WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(rule))
}

pub async fn update_response_rule(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateResponseRuleRequest>,
) -> Result<Json<ResponseRule>, AppError> {
    let existing = sqlx::query_as::<_, ResponseRule>(
        "SELECT * FROM response_rules WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Response rule not found".into()))?;

    let now = chrono::Utc::now().naive_utc();
    sqlx::query(
        "UPDATE response_rules SET name=$1, trigger_condition=$2, response_type=$3, response_content=$4, schedule=$5, is_active=$6, updated_at=$7 WHERE id=$8",
    )
    .bind(req.name.unwrap_or(existing.name))
    .bind(req.trigger_condition.unwrap_or(existing.trigger_condition))
    .bind(req.response_type.unwrap_or(existing.response_type))
    .bind(req.response_content.unwrap_or(existing.response_content))
    .bind(req.schedule.or(existing.schedule))
    .bind(req.is_active.unwrap_or(existing.is_active))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let rule = sqlx::query_as::<_, ResponseRule>("SELECT * FROM response_rules WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(rule))
}

pub async fn delete_response_rule(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM response_rules WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Response rule not found".into()));
    }
    Ok(Json(serde_json::json!({"message": "Response rule deleted"})))
}
