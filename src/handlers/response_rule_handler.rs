use axum::{
    extract::{Extension, Path, State},
    Json,
};
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    features,
    handlers::response_rule_eval::validate_rule,
    models::response_rule::{CreateResponseRuleRequest, ResponseRule, UpdateResponseRuleRequest},
    state::AppState,
};

/// The store's documented "no preference" priority: rules sharing it keep `created_at` order.
const DEFAULT_PRIORITY: i32 = 100;

/// Refuse a priority the evaluation order cannot read (the doc says "1 = highest").
fn validate_priority(priority: i32) -> Result<(), AppError> {
    if priority < 1 {
        return Err(AppError::BadRequest(format!(
            "priority must be 1 or greater (1 = highest), got {}",
            priority
        )));
    }
    Ok(())
}

/// Response Rules (kanban t_31f9cf38).
///
/// Listed in EVALUATION order — `priority` then `created_at` — which is the order
/// [`crate::handlers::response_rule_eval::run_for_inbound_call`] walks them on an inbound call, so
/// what the console shows is what the call path does. The shape of every write is validated against
/// the vocabulary the evaluator can actually read (see [`validate_rule`]), because a stored rule that
/// can never fire is exactly the defect this card closes: the consoles used to save
/// `response_type: "email"`/`"voice"` and an empty message body that nothing could ever act on.
pub async fn list_response_rules(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ResponseRule>>, AppError> {
    let rules = sqlx::query_as::<_, ResponseRule>(
        "SELECT * FROM response_rules WHERE tenant_id = $1 ORDER BY priority ASC, created_at ASC",
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
    validate_rule(
        &req.trigger_condition,
        &req.response_type,
        &req.response_content,
        req.schedule.as_ref(),
    )?;
    let priority = req.priority.unwrap_or(DEFAULT_PRIORITY);
    validate_priority(priority)?;

    features::enforce_feature_limit(&state.pool, claims.aid, "max_rules", "Response rules").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();
    let is_active = req.is_active.unwrap_or(true);

    sqlx::query(
        "INSERT INTO response_rules (id, name, trigger_condition, response_type, response_content, schedule, tenant_id, is_active, priority, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(id)
    .bind(&req.name)
    .bind(req.trigger_condition.trim())
    .bind(req.response_type.trim())
    .bind(&req.response_content)
    .bind(&req.schedule)
    .bind(claims.aid)
    .bind(is_active)
    .bind(priority)
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

    // Validate the MERGED rule, not just the fields the request happened to carry: a partial update
    // must not be able to leave a stored shape the evaluator cannot read.
    let trigger_condition = req
        .trigger_condition
        .unwrap_or(existing.trigger_condition)
        .trim()
        .to_string();
    let response_type = req
        .response_type
        .unwrap_or(existing.response_type)
        .trim()
        .to_string();
    let response_content = req.response_content.unwrap_or(existing.response_content);
    let schedule = req.schedule.or(existing.schedule);
    let priority = req.priority.unwrap_or(existing.priority);

    validate_rule(
        &trigger_condition,
        &response_type,
        &response_content,
        schedule.as_ref(),
    )?;
    validate_priority(priority)?;

    let now = chrono::Utc::now().naive_utc();
    sqlx::query(
        "UPDATE response_rules SET name=$1, trigger_condition=$2, response_type=$3, response_content=$4, schedule=$5, is_active=$6, priority=$7, updated_at=$8 WHERE id=$9",
    )
    .bind(req.name.unwrap_or(existing.name))
    .bind(&trigger_condition)
    .bind(&response_type)
    .bind(&response_content)
    .bind(&schedule)
    .bind(req.is_active.unwrap_or(existing.is_active))
    .bind(priority)
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
    Ok(Json(
        serde_json::json!({"message": "Response rule deleted"}),
    ))
}

/// `POST /api/v1/response-rules/preview` — "would my rule answer THIS caller, and what would it say?"
///
/// The console could describe a rule but never let the customer SEE it fire: with no phone number
/// connected, and no way to place a test call, the only proof a rule works was a real missed call
/// from a real customer. This answers the same question the inbound webhook asks, using the same
/// pure matcher ([`crate::handlers::response_rule_eval::trigger_matches`]) and the same renderer
/// ([`rule_text`]), against ONE caller number and the current clock — and it writes NOTHING: no
/// call log, no `messages` row, no provider request. The rules are read in the order the call path
/// walks them (priority, then created_at) and the first match is reported as the one that would
/// fire, because that is exactly what `run_for_inbound_call` does with the rest.
pub async fn preview_response_rule(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let caller = req
        .get("caller")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if caller.is_empty() {
        return Err(AppError::BadRequest(
            "Enter the caller's number to test against.".into(),
        ));
    }

    let rules = sqlx::query_as::<_, ResponseRule>(
        "SELECT * FROM response_rules
         WHERE tenant_id = $1 AND is_active = true
         ORDER BY priority ASC, created_at ASC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;

    let now = chrono::Utc::now().naive_utc();
    let mut evaluated: Vec<serde_json::Value> = Vec::new();
    let mut winner: Option<serde_json::Value> = None;
    for rule in &rules {
        let fires = crate::handlers::response_rule_eval::trigger_matches(rule, &caller, now);
        let text = crate::handlers::response_rule_eval::rule_text(&rule.response_content);
        let row = serde_json::json!({
            "id": rule.id.to_string(),
            "name": rule.name,
            "trigger_condition": rule.trigger_condition,
            "response_type": rule.response_type,
            "text": text,
            "priority": rule.priority,
            "matches": fires,
        });
        if fires && winner.is_none() {
            winner = Some(row.clone());
        }
        evaluated.push(row);
    }

    Ok(Json(serde_json::json!({
        "caller": caller,
        // The instant this answer is about. Published so the console can say WHEN it was tested
        // instead of implying the verdict is a permanent property of the rule.
        "evaluated_at": now.and_utc().to_rfc3339(),
        "rules_checked": rules.len(),
        // null => no active rule answers this caller right now; the console says so in words
        // rather than printing an empty card.
        "would_fire": winner,
        "rules": evaluated,
        "wrote": false,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_below_one_is_refused() {
        assert!(validate_priority(1).is_ok());
        assert!(validate_priority(DEFAULT_PRIORITY).is_ok());
        let e = validate_priority(0).unwrap_err();
        assert!(format!("{:?}", e).contains("priority"), "{:?}", e);
        assert!(validate_priority(-5).is_err());
    }
}
