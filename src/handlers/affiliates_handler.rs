//! Affiliates handler for MissedCall Respondr
//! DB-backed CRUD using the affiliates table.

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::NaiveDateTime;
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

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct Affiliate {
    pub id: String,
    pub tenant_id: Uuid,
    pub name: String,
    pub email: String,
    pub industry: Option<String>,
    pub commission_rate: Option<f64>,
    pub tax_docs: Option<Value>,
    pub is_active: bool,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Deserialize)]
pub struct CreateAffiliateRequest {
    pub name: String,
    pub email: String,
    pub industry: Option<String>,
    pub commission_rate: Option<f64>,
    pub tax_docs: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAffiliateRequest {
    pub name: Option<String>,
    pub email: Option<String>,
    pub industry: Option<String>,
    pub commission_rate: Option<f64>,
    pub tax_docs: Option<Value>,
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

fn generate_affiliate_id() -> String {
    let now = chrono::Utc::now();
    let date_part = now.format("%m%d%Y").to_string();
    let random_part: String = (0..5)
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
    let limit = query.limit.unwrap_or(50);
    let offset = query.offset.unwrap_or(0);

    let affiliates = if let Some(search) = &query.search {
        sqlx::query_as::<_, Affiliate>(
            r#"SELECT * FROM affiliates
               WHERE tenant_id = $1
                 AND (name ILIKE $2 OR email ILIKE $2)
               ORDER BY created_at DESC
               LIMIT $3 OFFSET $4"#,
        )
        .bind(claims.tenant_id)
        .bind(format!("%{}%", search))
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, Affiliate>(
            r#"SELECT * FROM affiliates
               WHERE tenant_id = $1
               ORDER BY created_at DESC
               LIMIT $2 OFFSET $3"#,
        )
        .bind(claims.tenant_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    };

    Ok(Json(json!({ "affiliates": affiliates })))
}

/// POST /api/v1/affiliates
pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateAffiliateRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let aff_id = generate_affiliate_id();

    sqlx::query(
        r#"INSERT INTO affiliates (id, tenant_id, name, email, industry, commission_rate, tax_docs)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
    )
    .bind(&aff_id)
    .bind(claims.tenant_id)
    .bind(&req.name)
    .bind(&req.email)
    .bind(&req.industry)
    .bind(req.commission_rate)
    .bind(&req.tax_docs)
    .execute(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": aff_id, "message": "Affiliate created" })),
    ))
}

/// GET /api/v1/affiliates/{id}
pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Affiliate>, AppError> {
    let affiliate = sqlx::query_as::<_, Affiliate>(
        r#"SELECT * FROM affiliates WHERE id = $1 AND tenant_id = $2"#,
    )
    .bind(&id)
    .bind(claims.tenant_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Affiliate not found".into()))?;

    Ok(Json(affiliate))
}

/// PUT /api/v1/affiliates/{id}
pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAffiliateRequest>,
) -> Result<Json<Value>, AppError> {
    let existing = sqlx::query_as::<_, Affiliate>(
        r#"SELECT * FROM affiliates WHERE id = $1 AND tenant_id = $2"#,
    )
    .bind(&id)
    .bind(claims.tenant_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Affiliate not found".into()))?;

    sqlx::query(
        r#"UPDATE affiliates
           SET name=$1, email=$2, industry=$3, commission_rate=$4,
               tax_docs=$5, is_active=$6, updated_at=NOW()
           WHERE id=$7 AND tenant_id=$8"#,
    )
    .bind(req.name.unwrap_or(existing.name))
    .bind(req.email.unwrap_or(existing.email))
    .bind(req.industry.or(existing.industry))
    .bind(req.commission_rate.or(existing.commission_rate))
    .bind(req.tax_docs.or(existing.tax_docs))
    .bind(req.is_active.unwrap_or(existing.is_active))
    .bind(&id)
    .bind(claims.tenant_id)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({ "message": "Affiliate updated" })))
}

/// DELETE /api/v1/affiliates/{id}
pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let result = sqlx::query(
        r#"DELETE FROM affiliates WHERE id = $1 AND tenant_id = $2"#,
    )
    .bind(&id)
    .bind(claims.tenant_id)
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Affiliate not found".into()));
    }

    Ok(Json(json!({ "message": "Affiliate deleted" })))
}
