use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;
use chrono::{DateTime, Utc};

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct Plan {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub price_monthly: f64,
    pub price_yearly: f64,
    pub features: Option<serde_json::Value>,
    pub is_active: bool,
    pub sort_order: i32,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

pub async fn list_plans(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT id, name, slug, description, price_monthly, price_yearly, features, is_active, sort_order, created_at, updated_at FROM plans ORDER BY sort_order ASC, price_monthly ASC"
    )
    .fetch_all(&state.pool)
    .await?;

    let plans: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "name": r.try_get::<String,_>("name").unwrap_or_default(),
            "slug": r.try_get::<String,_>("slug").unwrap_or_default(),
            "description": r.try_get::<Option<String>,_>("description").ok().flatten(),
            "price_monthly": r.try_get::<f64,_>("price_monthly").unwrap_or(0.0),
            "price_yearly": r.try_get::<f64,_>("price_yearly").unwrap_or(0.0),
            "features": r.try_get::<Option<serde_json::Value>,_>("features").ok().flatten(),
            "is_active": r.try_get::<bool,_>("is_active").unwrap_or(true),
            "sort_order": r.try_get::<i32, _>("sort_order").unwrap_or(0),
            "created_at": r.try_get::<Option<DateTime<Utc>>,_>("created_at").ok().flatten().map(|d| d.to_string()),
            "updated_at": r.try_get::<Option<DateTime<Utc>>,_>("updated_at").ok().flatten().map(|d| d.to_string()),
        })
    }).collect();

    Ok(Json(json!({"plans": plans, "total": plans.len()})))
}

pub async fn get_plan(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id, name, slug, description, price_monthly, price_yearly, features, is_active, sort_order, created_at, updated_at FROM plans WHERE id = $1"
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Plan not found".into()))?;

    Ok(Json(json!({"plan": {
        "id": row.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
        "name": row.try_get::<String,_>("name").unwrap_or_default(),
        "slug": row.try_get::<String,_>("slug").unwrap_or_default(),
        "description": row.try_get::<Option<String>,_>("description").ok().flatten(),
        "price_monthly": row.try_get::<f64,_>("price_monthly").unwrap_or(0.0),
        "price_yearly": row.try_get::<f64,_>("price_yearly").unwrap_or(0.0),
        "features": row.try_get::<Option<serde_json::Value>,_>("features").ok().flatten(),
        "is_active": row.try_get::<bool,_>("is_active").unwrap_or(true),
        "sort_order": row.try_get::<i32,_>("sort_order").unwrap_or(0),
    }})))
}

pub async fn create_plan(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let id = Uuid::new_v4();
    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let slug = req.get("slug").and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.to_lowercase().replace(' ', "-"));
    let description = req.get("description").and_then(|v| v.as_str()).map(|s| s.to_string());
    let price_monthly = req.get("price_monthly").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let price_yearly = req.get("price_yearly").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let features = req.get("features");
    let is_active = req.get("is_active").and_then(|v| v.as_bool()).unwrap_or(true);

    if name.is_empty() {
        return Err(AppError::BadRequest("Plan name is required".into()));
    }

    sqlx::query(
        r#"INSERT INTO plans (id, name, slug, description, price_monthly, price_yearly, features, is_active)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#
    )
    .bind(id)
    .bind(&name)
    .bind(&slug)
    .bind(&description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(features)
    .bind(is_active)
    .execute(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"id": id, "message": "Plan created"}))))
}

pub async fn update_plan(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    use sqlx::Row;
    let existing = sqlx::query(
        "SELECT id, name, slug, description, price_monthly, price_yearly, features, is_active, sort_order FROM plans WHERE id = $1"
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Plan not found".into()))?;

    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let name = if name.is_empty() { existing.try_get::<String,_>("name").unwrap_or_default() } else { name };
    let slug = req.get("slug").and_then(|v| v.as_str()).map(|s| s.to_string())
        .unwrap_or_else(|| existing.try_get::<String, _>("slug").unwrap_or_default());
    let description = req.get("description").and_then(|v| v.as_str()).map(|s| s.to_string());
    let description: Option<String> = description.or_else(|| existing.try_get::<Option<String>, _>("description").ok().flatten());
    let price_monthly = req.get("price_monthly").and_then(|v| v.as_f64()).unwrap_or_else(|| existing.try_get::<f64, _>("price_monthly").unwrap_or(0.0));
    let price_yearly = req.get("price_yearly").and_then(|v| v.as_f64()).unwrap_or_else(|| existing.try_get::<f64, _>("price_yearly").unwrap_or(0.0));
    let features = req.get("features").map(|v| v.clone()).or_else(|| existing.try_get::<Option<serde_json::Value>, _>("features").ok().flatten());
    let is_active = req.get("is_active").and_then(|v| v.as_bool()).unwrap_or_else(|| existing.try_get::<bool, _>("is_active").unwrap_or(true));

    sqlx::query(
        r#"UPDATE plans SET name=$1, slug=$2, description=$3, price_monthly=$4, price_yearly=$5,
           features=$6, is_active=$7 WHERE id=$8"#
    )
    .bind(&name)
    .bind(&slug)
    .bind(&description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(&features)
    .bind(is_active)
    .bind(id)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({"message": "Plan updated"})))
}

pub async fn delete_plan(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM plans WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Plan not found".into()));
    }

    Ok(Json(json!({"message": "Plan deleted"})))
}

pub async fn admin_update_plan_features(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let features = req.get("features").ok_or_else(|| AppError::BadRequest("features object required".into()))?;
    let features_str = features.to_string();
    sqlx::query(
        "UPDATE plans SET features = COALESCE(features::text, '{}')::jsonb || $1::jsonb, updated_at=NOW() WHERE id=$2"
    )
    .bind(&features_str)
    .bind(id)
    .execute(&state.pool)
    .await?;
    Ok(Json(json!({"message": "Features updated"})))
}

pub async fn admin_assign_plan(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let tenant_id = req.get("tenant_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("Valid tenant_id is required".into()))?;
    let plan_id = req.get("plan_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("Valid plan_id is required".into()))?;
    let billing_cycle = req.get("billing_cycle").and_then(|v| v.as_str()).unwrap_or("monthly");

    let tpid = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO tenant_plans (id, tenant_id, plan_id, status, billing_cycle)
           VALUES ($1, $2, $3, 'active', $4)
           ON CONFLICT (tenant_id) DO UPDATE SET plan_id=$3, status='active', updated_at=NOW()"#
    )
    .bind(tpid)
    .bind(tenant_id)
    .bind(plan_id)
    .bind(billing_cycle)
    .execute(&state.pool)
    .await?;
    Ok(Json(json!({"message": "Plan assigned to account"})))
}
