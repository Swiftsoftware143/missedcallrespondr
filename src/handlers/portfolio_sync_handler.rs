//! Internal portfolio sync handler — receives broadcasts from CoreSwift CRM.
//! Protected by x-internal-key header, not JWT.

use crate::error::AppError;
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, Json};
use serde_json::{json, Value};
use uuid::Uuid;

/// POST /api/v1/internal/portfolio-sync
pub async fn portfolio_sync_internal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let key = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if key != state.config.internal_sync_key {
        return Err(AppError::Unauthorized("Invalid internal key".into()));
    }

    let action = body
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("create");
    let portfolio_id = body
        .get("portfolio_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let tenant_id = body
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let slug = body
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    match action {
        "create" => {
            if let (Some(pid), Some(tid)) = (portfolio_id, tenant_id) {
                sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING")
                    .bind(tid).bind(&name).bind(&slug)
                    .execute(&state.pool).await.ok();
                sqlx::query("INSERT INTO portfolio_companies (id, tenant_id, name, slug) VALUES ($1, $2, $3, $4) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, slug = EXCLUDED.slug, updated_at = NOW()")
                    .bind(pid).bind(tid).bind(&name).bind(&slug)
                    .execute(&state.pool).await?;
            }
        }
        "update" => {
            if let Some(pid) = portfolio_id {
                let rows = sqlx::query("UPDATE portfolio_companies SET name = $1, slug = $2, updated_at = NOW() WHERE id = $3")
                    .bind(&name).bind(&slug).bind(pid)
                    .execute(&state.pool).await?;
                if rows.rows_affected() == 0 {
                    if let Some(tid) = tenant_id {
                        sqlx::query("INSERT INTO portfolio_companies (id, tenant_id, name, slug) VALUES ($1, $2, $3, $4) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, slug = EXCLUDED.slug, updated_at = NOW()")
                            .bind(pid).bind(tid).bind(&name).bind(&slug)
                            .execute(&state.pool).await?;
                    }
                }
            }
        }
        "delete" => {
            if let Some(pid) = portfolio_id {
                sqlx::query("DELETE FROM portfolio_companies WHERE id = $1")
                    .bind(pid)
                    .execute(&state.pool)
                    .await?;
            }
        }
        _ => return Err(AppError::BadRequest("Invalid action".into())),
    }

    Ok(Json(json!({"status": "synced"})))
}
