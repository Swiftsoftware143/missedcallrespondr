use axum::{
    extract::{Extension, Path, State},
    Json,
};
use serde_json::json;
use uuid::Uuid;

use sqlx::Row;

use crate::features;
use crate::{config::Claims, error::AppError, state::AppState};

pub async fn list_integration_targets(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let rows = sqlx::query(
        "SELECT id, name, provider, COALESCE(webhook_url, '') as webhook_url, api_key,
                events, is_active, portfolio_company_id, user_id, created_at, updated_at
         FROM integration_targets WHERE tenant_id = $1 ORDER BY name"
    )
    .bind(&claims.aid)
    .fetch_all(&state.pool)
    .await?;

    let targets: Vec<serde_json::Value> = rows.iter().map(|r| {
        let pc_id: Option<Uuid> = r.try_get("portfolio_company_id").ok();
        let uid: Option<Uuid> = r.try_get("user_id").ok();
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "name": r.try_get::<String,_>("name").unwrap_or_default(),
            "provider": r.try_get::<String,_>("provider").unwrap_or_default(),
            "webhook_url": r.try_get::<String,_>("webhook_url").unwrap_or_default(),
            "api_key": r.try_get::<String,_>("api_key").unwrap_or_default(),
            "events": r.try_get::<Vec<String>,_>("events").unwrap_or_default(),
            "is_active": r.try_get::<bool,_>("is_active").unwrap_or(true),
            "portfolio_company_id": pc_id.map(|u| u.to_string()),
            "user_id": uid.map(|u| u.to_string()),
        })
    }).collect();

    Ok(Json(targets))
}

pub async fn create_integration_target(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let tenant_id: Uuid = claims.aid;
    features::enforce_feature_limit(&state.pool, tenant_id, "max_integration_targets", "Integration Targets").await?;
    let name = body.get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("name is required".into()))?;
    let provider = body.get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("webhook");
    let webhook_url = body.get("webhook_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let api_key = body.get("api_key")
        .and_then(|v| v.as_str());
    let events: Vec<String> = body.get("events")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|e| e.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let portfolio_company_id: Option<Uuid> = body.get("portfolio_company_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());

    let id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO integration_targets (id, tenant_id, name, provider, webhook_url, api_key, events, portfolio_company_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7::text[], $8)"
    )
    .bind(id)
    .bind(&claims.aid)
    .bind(name)
    .bind(provider)
    .bind(webhook_url)
    .bind(api_key)
    .bind(&events)
    .bind(portfolio_company_id)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({
        "id": id.to_string(),
        "name": name,
        "provider": provider,
        "webhook_url": webhook_url,
        "api_key": api_key,
        "events": events,
        "is_active": true,
        "portfolio_company_id": portfolio_company_id.map(|u| u.to_string()),
    })))
}

pub async fn update_integration_target(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _existing = sqlx::query(
        "SELECT id FROM integration_targets WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id)
    .bind(&claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Integration target not found".into()))?;

    let name = body.get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let provider = body.get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let webhook_url = body.get("webhook_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let api_key = body.get("api_key")
        .and_then(|v| v.as_str());
    let events: Option<Vec<String>> = body.get("events")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|e| e.as_str().map(String::from)).collect());
    let is_active = body.get("is_active")
        .and_then(|v| v.as_bool());
    let portfolio_company_id: Option<Option<Uuid>> = if body.get("portfolio_company_id").is_some() {
        Some(body.get("portfolio_company_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok()))
    } else {
        None
    };

    sqlx::query(
        "UPDATE integration_targets SET
            name = COALESCE(NULLIF($1, ''), name),
            provider = COALESCE(NULLIF($2, ''), provider),
            webhook_url = COALESCE(NULLIF($3, ''), webhook_url),
            api_key = COALESCE($4, api_key),
            events = CASE WHEN $5::text[] = '{}'::text[] THEN events ELSE $5::text[] END,
            is_active = COALESCE($6, is_active),
            portfolio_company_id = COALESCE($7, portfolio_company_id),
            updated_at = NOW()
         WHERE id = $8"
    )
    .bind(name)
    .bind(provider)
    .bind(webhook_url)
    .bind(api_key)
    .bind(&events.unwrap_or_default())
    .bind(is_active)
    .bind(portfolio_company_id)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let row = sqlx::query(
        "SELECT id, name, provider, COALESCE(webhook_url, '') as webhook_url, api_key,
                events, is_active, portfolio_company_id, user_id
         FROM integration_targets WHERE id = $1"
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;

    let pc_id: Option<Uuid> = row.try_get("portfolio_company_id").ok();
    let uid: Option<Uuid> = row.try_get("user_id").ok();
    Ok(Json(json!({
        "id": row.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
        "name": row.try_get::<String,_>("name").unwrap_or_default(),
        "provider": row.try_get::<String,_>("provider").unwrap_or_default(),
        "webhook_url": row.try_get::<String,_>("webhook_url").unwrap_or_default(),
        "api_key": row.try_get::<String,_>("api_key").unwrap_or_default(),
        "events": row.try_get::<Vec<String>,_>("events").unwrap_or_default(),
        "is_active": row.try_get::<bool,_>("is_active").unwrap_or(true),
        "portfolio_company_id": pc_id.map(|u| u.to_string()),
        "user_id": uid.map(|u| u.to_string()),
    })))
}

pub async fn delete_integration_target(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM integration_targets WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(&claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Integration target not found".into()));
    }
    Ok(Json(json!({"message": "Integration target deleted"})))
}
