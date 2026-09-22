//! Affiliates handler for MissedCall Respondr
//! DB-backed CRUD using the affiliates table.
//!
//! Schema of record is this app's own migration `000011_schema_fix.sql`:
//!   id uuid PK, tenant_id uuid NOT NULL, user_id uuid NULL, code varchar(64) UNIQUE,
//!   commission_rate numeric(6,4) NOT NULL, is_active bool NOT NULL,
//!   created_at timestamptz NOT NULL.
//!
//! This file previously described a *FunnelSwift* affiliates table (text `id`,
//! `name`/`email`/`industry`/`tax_docs`/`updated_at`) — cross-app bleed. Against
//! this app's real table that meant: `GET /affiliates/{id}` 500'd with
//! `operator does not exist: uuid = text` on every call, and `POST /affiliates`
//! could not even deserialize (it demanded `name`, which no column holds). The
//! decode types below are the real column types.

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::FromRow;
use uuid::Uuid;

use crate::config::Claims;
use crate::error::AppError;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// Every statement below selects the real schema's columns explicitly and casts
/// `commission_rate::float8` — `commission_rate` is `numeric` and sqlx has no `f64`
/// decode for NUMERIC, the same convention `plans_handler.rs` uses for
/// `plans.price_monthly`. The column list is written out at each call site rather
/// than built from a constant, because the fleet decode-type audit
/// (`/opt/swift/bin/fleet-dbtype-audit.py`) only honours a `col::cast` it can see
/// inside the statement's own literal.
#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct Affiliate {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Option<Uuid>,
    pub code: String,
    pub commission_rate: f64,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

/// `deny_unknown_fields` is deliberate: the old contract accepted `name`/`email`
/// and silently had nowhere to put them. A caller that sends a field this table
/// cannot store now gets a 422 naming it, instead of a 201 that lied.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAffiliateRequest {
    pub code: Option<String>,
    pub user_id: Option<Uuid>,
    pub commission_rate: Option<f64>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateAffiliateRequest {
    pub code: Option<String>,
    pub user_id: Option<Uuid>,
    pub commission_rate: Option<f64>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub search: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A unique referral code. `code` is UNIQUE across the table, and 36^10 of
/// namespace makes a collision a non-event rather than a retry loop.
fn generate_affiliate_code() -> String {
    let now = chrono::Utc::now();
    let date_part = now.format("%m%d%Y").to_string();
    let random_part: String = (0..10)
        .map(|_| {
            let n = rand::random::<u8>() % 36;
            if n < 10 {
                (b'0' + n) as char
            } else {
                (b'A' + n - 10) as char
            }
        })
        .collect();
    format!("AFF-{}-{}", date_part, random_part)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/v1/affiliates
pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, AppError> {
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let offset = query.offset.unwrap_or(0).max(0);

    let affiliates = if let Some(search) = &query.search {
        sqlx::query_as::<_, Affiliate>(
            "SELECT id, tenant_id, user_id, code, commission_rate::float8 AS commission_rate, \
             is_active, created_at FROM affiliates \
             WHERE tenant_id = $1 AND code ILIKE $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid)
        .bind(format!("%{}%", search))
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, Affiliate>(
            "SELECT id, tenant_id, user_id, code, commission_rate::float8 AS commission_rate, \
             is_active, created_at FROM affiliates \
             WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(claims.aid)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    };

    Ok(Json(
        json!({ "affiliates": affiliates, "total": affiliates.len() }),
    ))
}

/// POST /api/v1/affiliates
pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateAffiliateRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let code = req
        .code
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(generate_affiliate_code);
    let commission_rate = req.commission_rate.unwrap_or(0.0);
    if !(0.0..=1.0).contains(&commission_rate) {
        return Err(AppError::BadRequest(
            "commission_rate must be between 0 and 1".into(),
        ));
    }
    let is_active = req.is_active.unwrap_or(true);

    let created = sqlx::query_as::<_, Affiliate>(
        "INSERT INTO affiliates (tenant_id, user_id, code, commission_rate, is_active) \
         VALUES ($1, $2, $3, $4, $5) \
         RETURNING id, tenant_id, user_id, code, \
                   commission_rate::float8 AS commission_rate, is_active, created_at",
    )
    .bind(claims.aid)
    .bind(req.user_id)
    .bind(&code)
    .bind(commission_rate)
    .bind(is_active)
    .fetch_one(&state.pool)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref db) if db.constraint() == Some("affiliates_code_key") => {
            AppError::Conflict(format!("Affiliate code '{}' already exists", code))
        }
        other => AppError::from(other),
    })?;

    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": created.id, "affiliate": created, "message": "Affiliate created" })),
    ))
}

/// GET /api/v1/affiliates/{id}
pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Affiliate>, AppError> {
    let affiliate = sqlx::query_as::<_, Affiliate>(
        "SELECT id, tenant_id, user_id, code, commission_rate::float8 AS commission_rate, \
         is_active, created_at FROM affiliates WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Affiliate not found".into()))?;

    Ok(Json(affiliate))
}

/// PUT /api/v1/affiliates/{id}
pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateAffiliateRequest>,
) -> Result<Json<Value>, AppError> {
    if let Some(rate) = req.commission_rate {
        if !(0.0..=1.0).contains(&rate) {
            return Err(AppError::BadRequest(
                "commission_rate must be between 0 and 1".into(),
            ));
        }
    }

    let updated = sqlx::query_as::<_, Affiliate>(
        "UPDATE affiliates SET \
         code = COALESCE($1, code), \
         user_id = COALESCE($2, user_id), \
         commission_rate = COALESCE($3, commission_rate), \
         is_active = COALESCE($4, is_active) \
         WHERE id = $5 AND tenant_id = $6 \
         RETURNING id, tenant_id, user_id, code, \
                   commission_rate::float8 AS commission_rate, is_active, created_at",
    )
    .bind(req.code.as_deref().map(str::trim).filter(|c| !c.is_empty()))
    .bind(req.user_id)
    .bind(req.commission_rate)
    .bind(req.is_active)
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref db) if db.constraint() == Some("affiliates_code_key") => {
            AppError::Conflict("Affiliate code already exists".into())
        }
        other => AppError::from(other),
    })?
    .ok_or_else(|| AppError::NotFound("Affiliate not found".into()))?;

    Ok(Json(
        json!({ "affiliate": updated, "message": "Affiliate updated" }),
    ))
}

/// DELETE /api/v1/affiliates/{id}
pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let result = sqlx::query("DELETE FROM affiliates WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Affiliate not found".into()));
    }

    Ok(Json(json!({ "message": "Affiliate deleted" })))
}
