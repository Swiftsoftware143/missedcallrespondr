use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize, FromRow)]
#[allow(dead_code)]
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
    pub payment_provider: Option<String>,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

// ---------------------------------------------------------------------------
// Decode helpers
//
// `plans.created_at` / `updated_at` are `timestamp without time zone` and
// `price_monthly` / `price_yearly` are `numeric`. Decoding the timestamps as
// `DateTime<Utc>` and the numerics as `f64` fails in sqlx, and every call site
// used to swallow that failure (`try_get(..).ok()` / `.unwrap_or(0.0)`), so the
// admin plans table rendered NULL dates and $0.00 prices for four live rows with
// no log line and no 500. The types below match the columns; a decode failure is
// now logged instead of collapsing into the chrono/serde default.
// ---------------------------------------------------------------------------

/// A `timestamp without time zone` column, emitted as RFC3339 UTC. This app
/// writes these with `NOW()` from a UTC session, so the naive value is UTC.
fn ts_utc(row: &sqlx::postgres::PgRow, col: &str) -> Option<String> {
    use sqlx::Row;
    match row.try_get::<Option<NaiveDateTime>, _>(col) {
        Ok(v) => v.map(|d| d.and_utc().to_rfc3339()),
        Err(e) => {
            tracing::warn!(column = col, error = %e, "plans: timestamp decode failed");
            None
        }
    }
}

/// A `numeric` money column. sqlx has no `f64` decode for NUMERIC, so every
/// SELECT below casts the column to `float8` — the same convention this file
/// already uses in `attribute_plan_upgrade`. If that cast is ever dropped, this
/// says so instead of reporting $0.00 for a paid plan.
fn money(row: &sqlx::postgres::PgRow, col: &str) -> f64 {
    use sqlx::Row;
    match row.try_get::<Option<f64>, _>(col) {
        Ok(Some(v)) => v,
        Ok(None) => 0.0,
        Err(e) => {
            tracing::warn!(
                column = col,
                error = %e,
                "plans: numeric decode failed — the SELECT must cast this column to float8"
            );
            0.0
        }
    }
}

pub async fn list_plans(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    use sqlx::Row;
    // NOTE: price_monthly/price_yearly are `numeric`; the ::float8 cast is what
    // makes them decodable as f64 (see `money`).
    let rows = sqlx::query(
        "SELECT id, name, slug, description, price_monthly::float8 AS price_monthly, \
         price_yearly::float8 AS price_yearly, features, is_active, sort_order, payment_provider, \
         created_at, updated_at FROM plans ORDER BY sort_order ASC, price_monthly ASC",
    )
    .fetch_all(&state.pool)
    .await?;

    let plans: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "name": r.try_get::<String,_>("name").unwrap_or_default(),
            "slug": r.try_get::<String,_>("slug").unwrap_or_default(),
            "description": r.try_get::<Option<String>,_>("description").ok().flatten(),
            "price_monthly": money(r, "price_monthly"),
            "price_yearly": money(r, "price_yearly"),
            "features": r.try_get::<Option<serde_json::Value>,_>("features").ok().flatten(),
            "is_active": r.try_get::<bool,_>("is_active").unwrap_or(true),
            "sort_order": r.try_get::<i32, _>("sort_order").unwrap_or(0),
            "payment_provider": r.try_get::<Option<String>,_>("payment_provider").ok().flatten(),
            "created_at": ts_utc(r, "created_at"),
            "updated_at": ts_utc(r, "updated_at"),
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
        "SELECT id, name, slug, description, price_monthly::float8 AS price_monthly, \
         price_yearly::float8 AS price_yearly, features, is_active, sort_order, payment_provider, \
         created_at, updated_at FROM plans WHERE id = $1",
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
        "price_monthly": money(&row, "price_monthly"),
        "price_yearly": money(&row, "price_yearly"),
        "features": row.try_get::<Option<serde_json::Value>,_>("features").ok().flatten(),
        "is_active": row.try_get::<bool,_>("is_active").unwrap_or(true),
        "sort_order": row.try_get::<i32,_>("sort_order").unwrap_or(0),
        "payment_provider": row.try_get::<Option<String>,_>("payment_provider").ok().flatten(),
        "created_at": ts_utc(&row, "created_at"),
        "updated_at": ts_utc(&row, "updated_at"),
    }})))
}

pub async fn create_plan(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let id = Uuid::new_v4();
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let slug = req
        .get("slug")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| name.to_lowercase().replace(' ', "-"));
    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let price_monthly = req
        .get("price_monthly")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let price_yearly = req
        .get("price_yearly")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let features = req.get("features");
    let is_active = req
        .get("is_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if name.is_empty() {
        return Err(AppError::BadRequest("Plan name is required".into()));
    }

    let payment_provider = req
        .get("payment_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    sqlx::query(
        r#"INSERT INTO plans (id, name, slug, description, price_monthly, price_yearly, features, is_active, payment_provider)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#
    )
    .bind(id)
    .bind(&name)
    .bind(&slug)
    .bind(&description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(features)
    .bind(is_active)
    .bind(&payment_provider)
    .execute(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({"id": id, "message": "Plan created"})),
    ))
}

pub async fn update_plan(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    use sqlx::Row;
    let existing = sqlx::query(
        "SELECT id, name, slug, description, price_monthly::float8 AS price_monthly, \
         price_yearly::float8 AS price_yearly, features, is_active, sort_order FROM plans WHERE id = $1"
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Plan not found".into()))?;

    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name = if name.is_empty() {
        existing.try_get::<String, _>("name").unwrap_or_default()
    } else {
        name
    };
    let slug = req
        .get("slug")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| existing.try_get::<String, _>("slug").unwrap_or_default());
    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let description: Option<String> = description.or_else(|| {
        existing
            .try_get::<Option<String>, _>("description")
            .ok()
            .flatten()
    });
    let price_monthly = req
        .get("price_monthly")
        .and_then(|v| v.as_f64())
        .unwrap_or_else(|| money(&existing, "price_monthly"));
    let price_yearly = req
        .get("price_yearly")
        .and_then(|v| v.as_f64())
        .unwrap_or_else(|| money(&existing, "price_yearly"));
    let features = req.get("features").cloned().or_else(|| {
        existing
            .try_get::<Option<serde_json::Value>, _>("features")
            .ok()
            .flatten()
    });
    let is_active = req
        .get("is_active")
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| existing.try_get::<bool, _>("is_active").unwrap_or(true));
    let payment_provider: Option<String> = req
        .get("payment_provider")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            existing
                .try_get::<Option<String>, _>("payment_provider")
                .ok()
                .flatten()
        });

    sqlx::query(
        r#"UPDATE plans SET name=$1, slug=$2, description=$3, price_monthly=$4, price_yearly=$5,
           features=$6, is_active=$7, payment_provider=$8 WHERE id=$9"#,
    )
    .bind(&name)
    .bind(&slug)
    .bind(&description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(&features)
    .bind(is_active)
    .bind(&payment_provider)
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
    let features = req
        .get("features")
        .ok_or_else(|| AppError::BadRequest("features object required".into()))?;
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

/// Fire-and-forget notification to FunnelSwift that a tenant upgraded to a paid plan,
/// so the referring affiliate is credited (permanent, no expiry).
async fn notify_funnelswift_upgrade(
    funnelswift_url: &str,
    internal_sync_key: &str,
    email: &str,
    plan_name: &str,
    plan_price: f64,
    event_id: &str,
) {
    if email.is_empty() || funnelswift_url.is_empty() {
        return;
    }
    let url = format!(
        "{}/api/v1/internal/affiliate/upgrade-event",
        funnelswift_url.trim_end_matches('/')
    );
    let key = internal_sync_key.to_string();
    let payload = serde_json::json!({
        "source_app": "missedcallrespondr",
        "email": email,
        "plan_name": plan_name,
        "plan_price": plan_price,
        "event_id": event_id,
    });
    tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(&url)
            .header("x-internal-key", key)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;
    });
}

/// Resolve the tenant's owner email + plan, and notify FunnelSwift if it's a PAID upgrade.
async fn attribute_plan_upgrade(state: &AppState, tenant_id: Uuid, plan_id: Uuid) {
    let plan: Option<(String, Option<f64>)> =
        match sqlx::query_as("SELECT name, price_monthly::float8 FROM plans WHERE id = $1")
            .bind(plan_id)
            .fetch_optional(&state.pool)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(
                    "plans.attribute_plan_upgrade: plan lookup failed (plan_id={plan_id}): {e}"
                );
                return;
            }
        };
    let Some((plan_name, price)) = plan else {
        return;
    };
    let plan_price = price.unwrap_or(0.0);
    if plan_price <= 0.0 {
        return;
    }
    let email: Option<String> = match sqlx::query_scalar(
        "SELECT email FROM users WHERE tenant_id = $1 AND role IN ('admin','company_admin','account_owner') LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(e) => e,
        Err(e) => {
            tracing::error!(
                "plans.attribute_plan_upgrade: owner email lookup failed (tenant_id={tenant_id}): {e}"
            );
            return;
        }
    };
    let Some(email) = email else {
        return;
    };
    notify_funnelswift_upgrade(
        &state.funnelswift_url,
        &state.config.internal_sync_key,
        &email,
        &plan_name,
        plan_price,
        &Uuid::new_v4().to_string(),
    )
    .await;
}

pub async fn admin_assign_plan(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let tenant_id = req
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("Valid tenant_id is required".into()))?;
    let plan_id = req
        .get("plan_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("Valid plan_id is required".into()))?;
    let billing_cycle = req
        .get("billing_cycle")
        .and_then(|v| v.as_str())
        .unwrap_or("monthly");

    let tpid = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO tenant_plans (id, tenant_id, plan_id, status, billing_cycle)
           VALUES ($1, $2, $3, 'active', $4)
           ON CONFLICT (tenant_id) DO UPDATE SET plan_id=$3, status='active', updated_at=NOW()"#,
    )
    .bind(tpid)
    .bind(tenant_id)
    .bind(plan_id)
    .bind(billing_cycle)
    .execute(&state.pool)
    .await?;

    // Credit the referring affiliate if this is a paid-plan assignment.
    attribute_plan_upgrade(&state, tenant_id, plan_id).await;

    Ok(Json(json!({"message": "Plan assigned to account"})))
}

#[cfg(test)]
mod swallow_proof {
    //! t_08faed51: `attribute_plan_upgrade` used to run its two lookups behind
    //! `.ok().flatten()`, so a DB failure returned silently instead of logging. Its only
    //! live caller (`admin_assign_plan`) writes `tenant_plans` first, and no lock can
    //! block this lookup without blocking that write, so the failure is forced here
    //! against a Postgres that genuinely refuses the connection.
    use super::*;
    use crate::handlers::swallow_tests::{capture, dead_pool, test_state};

    #[test]
    fn attribute_plan_upgrade_logs_a_failed_plan_lookup() {
        let (_out, log) = capture(|| {
            let state: AppState = test_state();
            async move {
                attribute_plan_upgrade(&state, Uuid::nil(), Uuid::nil()).await;
            }
        });
        assert!(
            log.contains("attribute_plan_upgrade: plan lookup failed"),
            "the failed lookup must be logged: {log}"
        );
    }

    #[test]
    fn attribute_plan_upgrade_old_shape_was_silent() {
        // control leg: the exact pre-fix expression on the same dead pool.
        let (plan, log) = capture(|| {
            let pool = dead_pool();
            async move {
                let p: Option<(String, Option<f64>)> =
                    sqlx::query_as("SELECT name, price_monthly::float8 FROM plans WHERE id = $1")
                        .bind(Uuid::nil())
                        .fetch_optional(&pool)
                        .await
                        .ok()
                        .flatten();
                p
            }
        });
        assert!(
            plan.is_none(),
            "control: the pre-fix shape returned None for a failed query"
        );
        assert!(
            !log.contains("attribute_plan_upgrade"),
            "control: the pre-fix shape logged nothing at all: {log}"
        );
    }
}
