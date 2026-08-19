use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::deal::{CreateDealRequest, Deal, MoveDealRequest, UpdateDealRequest},
    state::AppState,
};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub stage: Option<String>,
    pub search: Option<String>,
}

pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Deal>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = if let (Some(stage), Some(search)) = (&q.stage, &q.search) {
        let pat = format!("%{}%", search);
        sqlx::query_as::<_, Deal>(
            "SELECT * FROM deals WHERE tenant_id = $1 AND stage = $2 AND (name ILIKE $3) ORDER BY created_at DESC LIMIT $4 OFFSET $5",
        )
        .bind(claims.aid).bind(stage).bind(&pat).bind(limit).bind(offset).fetch_all(&state.pool).await?
    } else if let Some(stage) = &q.stage {
        sqlx::query_as::<_, Deal>(
            "SELECT * FROM deals WHERE tenant_id = $1 AND stage = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid).bind(stage).bind(limit).bind(offset).fetch_all(&state.pool).await?
    } else if let Some(search) = &q.search {
        let pat = format!("%{}%", search);
        sqlx::query_as::<_, Deal>(
            "SELECT * FROM deals WHERE tenant_id = $1 AND name ILIKE $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid).bind(&pat).bind(limit).bind(offset).fetch_all(&state.pool).await?
    } else {
        sqlx::query_as::<_, Deal>(
            "SELECT * FROM deals WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(claims.aid)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    };
    Ok(Json(items))
}

pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateDealRequest>,
) -> Result<Json<Deal>, AppError> {
    crate::features::enforce_feature_limit(&state.pool, claims.aid, "max_deals", "Deals").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO deals (id, tenant_id, name, contact_id, lead_id, value, stage, probability, expected_close_date, is_won, source, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.name)
    .bind(req.contact_id)
    .bind(req.lead_id)
    .bind(req.value.unwrap_or_else(|| rust_decimal::Decimal::new(0, 0)))
    .bind(req.stage.as_deref().unwrap_or("new"))
    .bind(req.probability.unwrap_or(10))
    .bind(req.expected_close_date)
    .bind(req.is_won.unwrap_or(false))
    .bind(req.source.as_deref().unwrap_or("call"))
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Deal>("SELECT * FROM deals WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Deal>, AppError> {
    let item = sqlx::query_as::<_, Deal>("SELECT * FROM deals WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Deal not found".into()))?;
    Ok(Json(item))
}

pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateDealRequest>,
) -> Result<Json<Deal>, AppError> {
    let existing =
        sqlx::query_as::<_, Deal>("SELECT * FROM deals WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Deal not found".into()))?;
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE deals SET name=$1, contact_id=$2, lead_id=$3, value=$4, stage=$5, probability=$6, expected_close_date=$7, is_won=$8, source=$9, updated_at=$10 WHERE id=$11",
    )
    .bind(req.name.as_ref().unwrap_or(&existing.name))
    .bind(req.contact_id.or(existing.contact_id))
    .bind(req.lead_id.or(existing.lead_id))
    .bind(req.value.unwrap_or(existing.value))
    .bind(req.stage.as_ref().unwrap_or(&existing.stage))
    .bind(req.probability.unwrap_or(existing.probability))
    .bind(req.expected_close_date.or(existing.expected_close_date))
    .bind(req.is_won.unwrap_or(existing.is_won))
    .bind(req.source.as_ref().unwrap_or(&existing.source))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Deal>("SELECT * FROM deals WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

// Move deal to a stage (pipeline drag / stage update)
pub async fn move_stage(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<MoveDealRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let existing =
        sqlx::query_as::<_, Deal>("SELECT * FROM deals WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Deal not found".into()))?;
    let now = chrono::Utc::now();
    sqlx::query("UPDATE deals SET stage=$1, updated_at=$2 WHERE id=$3")
        .bind(&req.stage)
        .bind(now)
        .bind(id)
        .execute(&state.pool)
        .await?;
    // stage progression → won
    let won = req.stage.eq_ignore_ascii_case("won");
    if won && !existing.is_won {
        sqlx::query("UPDATE deals SET is_won=true, updated_at=$1 WHERE id=$2")
            .bind(now)
            .bind(id)
            .execute(&state.pool)
            .await?;
    }
    Ok(Json(serde_json::json!({
        "id": id,
        "stage": req.stage,
        "is_won": won || existing.is_won,
        "message": "Deal moved"
    })))
}

pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM deals WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Deal not found".into()));
    }
    Ok(Json(serde_json::json!({"deleted": true, "id": id})))
}
