//! Internal portfolio sync handler — receives broadcasts from CoreSwift CRM.
//! Protected by x-internal-key header, not JWT.

use crate::error::AppError;
use crate::state::AppState;
use crate::validation::{check_len, max};
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
    // Fail closed on an EMPTY configured key: `key != expected` alone authorises a request that
    // simply omits the header on any host where INTERNAL_SYNC_KEY is unset (class t_eb7736b8).
    if state.config.internal_sync_key.is_empty() || key != state.config.internal_sync_key {
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

    check_len("name", &name, max::PORTFOLIO_COMPANIES_NAME)?;
    check_len("slug", &slug, max::PORTFOLIO_COMPANIES_SLUG)?;

    match action {
        "create" => {
            if let (Some(pid), Some(tid)) = (portfolio_id, tenant_id) {
                // `tenants.slug` carries UNIQUE `tenants_slug_key`, and the slug on this arm is
                // CALLER-SUPPLIED (`body.slug`), so a slug that already belongs to any tenant made the
                // tenants INSERT raise 23505 — and `.ok()` DISCARDED it. The next statement then wrote
                // a `portfolio_companies` row whose `tenant_id` had no parent, so the FK
                // (`portfolio_companies_tenant_id_fkey`) raised and the caller got an opaque
                // `500 {"error":"Database error"}` naming nothing (kanban t_4bccea82, reproduced live
                // 2026-10-08T17:22:22Z). The two statements now run in ONE transaction and the error is
                // PROPAGATED: a caller-supplied slug is honoured when it is actually free and refused
                // with a 409 that names it when it is taken — never silently replaced, because the
                // hub's contract is that `body.slug` is what lands in both rows. A create that supplies
                // NO slug is derived by the app's own rule, the one every account door uses
                // (kanban t_1a26f923 / t_ff66fbe3).
                let tenant_slug = if slug.trim().is_empty() {
                    crate::auth::signup::unique_account_slug(&state.pool, &name).await?
                } else {
                    slug.clone()
                };

                let mut tx = state.pool.begin().await?;
                sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING")
                    .bind(tid).bind(&name).bind(&tenant_slug)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| match e {
                        // The only unique guard this statement can trip which its arbiter does not
                        // absorb is `tenants_slug_key`: a PK conflict is the ON CONFLICT target.
                        // Matched on the SQLSTATE, not on a constraint name (write-validation-parity).
                        sqlx::Error::Database(db)
                            if db.code().as_deref() == Some("23505") =>
                        {
                            AppError::Conflict(format!(
                                "slug '{}' is already in use by another account",
                                tenant_slug
                            ))
                        }
                        other => other.into(),
                    })?;
                sqlx::query("INSERT INTO portfolio_companies (id, tenant_id, name, slug) VALUES ($1, $2, $3, $4) ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, slug = EXCLUDED.slug, updated_at = NOW()")
                    .bind(pid).bind(tid).bind(&name).bind(&tenant_slug)
                    .execute(&mut *tx).await?;
                tx.commit().await?;
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
