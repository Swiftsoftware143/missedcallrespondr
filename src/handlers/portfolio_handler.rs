use axum::{
    extract::{Extension, Path, State},
    http::HeaderMap,
    Json,
};
use serde_json::json;
use uuid::Uuid;

use sqlx::Row;

use crate::features;
use crate::{config::Claims, error::AppError, state::AppState};

pub async fn list_portfolio_companies(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let rows = sqlx::query(
        "SELECT id, name, slug, settings::text, is_active, created_at, updated_at FROM portfolio_companies WHERE tenant_id = $1 ORDER BY name"
    )
    .bind(&claims.aid)
    .fetch_all(&state.pool)
    .await?;

    let companies: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "name": r.try_get::<String,_>("name").unwrap_or_default(),
            "slug": r.try_get::<String,_>("slug").unwrap_or_default(),
            "settings": r.try_get::<String,_>("settings").unwrap_or_else(|_| "{}".into()),
            "is_active": r.try_get::<bool,_>("is_active").unwrap_or(true),
        })
    }).collect();

    Ok(Json(companies))
}

pub async fn create_portfolio_company(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let tenant_id: Uuid = claims.aid;
    features::enforce_feature_limit(&state.pool, tenant_id, "max_portfolio_companys", "Portfolio Companys").await?;
    let name = body.get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("name is required".into()))?;
    let slug = body.get("slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("slug is required".into()))?;
    let settings = body.get("settings").map(|v| v.to_string()).unwrap_or_else(|| "{}".into());

    let id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO portfolio_companies (id, tenant_id, name, slug, settings) VALUES ($1, $2, $3, $4, $5::jsonb)"
    )
    .bind(id)
    .bind(&claims.aid)
    .bind(name)
    .bind(slug)
    .bind(&settings)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({
        "id": id.to_string(),
        "name": name,
        "slug": slug,
        "settings": settings,
        "is_active": true,
    })))
}

pub async fn get_portfolio_company(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row = sqlx::query(
        "SELECT id, name, slug, settings::text, is_active, created_at, updated_at FROM portfolio_companies WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id)
    .bind(&claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Portfolio company not found".into()))?;

    Ok(Json(json!({
        "id": row.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
        "name": row.try_get::<String,_>("name").unwrap_or_default(),
        "slug": row.try_get::<String,_>("slug").unwrap_or_default(),
        "settings": row.try_get::<String,_>("settings").unwrap_or_else(|_| "{}".into()),
        "is_active": row.try_get::<bool,_>("is_active").unwrap_or(true),
    })))
}

pub async fn update_portfolio_company(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let _existing = sqlx::query(
        "SELECT id FROM portfolio_companies WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id)
    .bind(&claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Portfolio company not found".into()))?;

    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let slug = body.get("slug").and_then(|v| v.as_str()).unwrap_or("");
    let settings_str = body.get("settings").map(|v| v.to_string()).unwrap_or_else(|| "{}".into());
    let is_active = body.get("is_active").and_then(|v| v.as_bool()).unwrap_or(true);

    sqlx::query(
        "UPDATE portfolio_companies SET name = COALESCE(NULLIF($1, ''), name), slug = COALESCE(NULLIF($2, ''), slug), settings = CASE WHEN $3::jsonb = '{}'::jsonb THEN settings ELSE $3::jsonb END, is_active = $4, updated_at = NOW() WHERE id = $5"
    )
    .bind(name)
    .bind(slug)
    .bind(&settings_str)
    .bind(is_active)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let row = sqlx::query(
        "SELECT id, name, slug, settings::text, is_active FROM portfolio_companies WHERE id = $1"
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(json!({
        "id": row.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
        "name": row.try_get::<String,_>("name").unwrap_or_default(),
        "slug": row.try_get::<String,_>("slug").unwrap_or_default(),
        "settings": row.try_get::<String,_>("settings").unwrap_or_else(|_| "{}".into()),
        "is_active": row.try_get::<bool,_>("is_active").unwrap_or(true),
    })))
}

/// POST /api/v1/internal/portfolio-companies — internal sync, no JWT
pub async fn internal_create_portfolio_company(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let key = headers.get("x-internal-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    if key != state.config.internal_sync_key {
        return Err(AppError::Unauthorized("Invalid internal key".into()));
    }

    let tenant_id = body.get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("tenant_id required".into()))?;

    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("Company").to_string();
    let slug = body.get("slug").and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_else(|| name.to_lowercase().replace(' ', "-"));
    let email = body.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let description = body.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let id = Uuid::new_v4();

    // Ensure tenant exists (FK constraint)
    sqlx::query(
        "INSERT INTO tenants (id, name, slug) VALUES ($1, $2, CONCAT($3, '-', LEFT(CAST($1 AS TEXT), 8))) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, slug = EXCLUDED.slug"
    )
    .bind(tenant_id)
    .bind(&name)
    .bind(&slug)
    .execute(&state.pool)
    .await.ok();

    let settings = json!({
        "email": email,
        "description": description,
    }).to_string();

    sqlx::query(
        "INSERT INTO portfolio_companies (id, tenant_id, name, slug, settings) VALUES ($1, $2, $3, $4, $5::jsonb) ON CONFLICT (id) DO NOTHING"
    )
    .bind(id)
    .bind(tenant_id)
    .bind(&name)
    .bind(&slug)
    .bind(&settings)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({"status": "synced", "id": id.to_string()})))
}

pub async fn delete_portfolio_company(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM portfolio_companies WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(&claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Portfolio company not found".into()));
    }
    Ok(Json(json!({"message": "Portfolio company deleted"})))
}
