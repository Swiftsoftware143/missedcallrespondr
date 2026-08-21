//! Lists handler — each MissedCall campaign owns its own fresh list (named after the
//! campaign, e.g. campaign "Inbound Plumbing" -> list "Inbound Plumbing"). Also supports
//! standalone lists for ad-hoc lead organization. Lists track lead membership so that
//! leads carrying a campaign's linked tag flow into that campaign's list.

use axum::extract::{Extension, Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::list::{CreateListRequest, List, ListLead, ListLeadRequest, UpdateListRequest},
    state::AppState,
};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub campaign_id: Option<Uuid>,
}

/// GET /api/v1/lists — all lists for the tenant, optionally filtered by campaign.
pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<List>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = if let Some(cid) = q.campaign_id {
        sqlx::query_as::<_, List>(
            "SELECT * FROM lists WHERE tenant_id = $1 AND campaign_id = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid)
        .bind(cid)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, List>(
            "SELECT * FROM lists WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(claims.aid)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    };
    Ok(Json(items))
}

/// GET /api/v1/lists/:id/leads — the leads currently in a list.
pub async fn list_leads(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ListLead>>, AppError> {
    // Verify list belongs to tenant.
    let owned: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM lists WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?;
    if owned.is_none() {
        return Err(AppError::NotFound("List not found".into()));
    }
    let items = sqlx::query_as::<_, ListLead>(
        "SELECT l.id, l.name, l.phone, l.email, l.status, l.tags, ll.added_at
         FROM list_leads ll
         JOIN leads l ON l.id = ll.lead_id
         WHERE ll.list_id = $1
         ORDER BY ll.added_at DESC",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

/// POST /api/v1/lists/:id/leads — add a lead to a list (idempotent).
pub async fn add_lead(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<ListLeadRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let owned: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM lists WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?;
    if owned.is_none() {
        return Err(AppError::NotFound("List not found".into()));
    }
    sqlx::query(
        "INSERT INTO list_leads (list_id, lead_id) VALUES ($1, $2)
         ON CONFLICT (list_id, lead_id) DO NOTHING",
    )
    .bind(id)
    .bind(req.lead_id)
    .execute(&state.pool)
    .await?;
    Ok(Json(
        json!({ "added": true, "list_id": id, "lead_id": req.lead_id }),
    ))
}

/// DELETE /api/v1/lists/:id/leads/:lead_id — remove a lead from a list.
pub async fn remove_lead(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path((list_id, lead_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let owned: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM lists WHERE id = $1 AND tenant_id = $2")
            .bind(list_id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?;
    if owned.is_none() {
        return Err(AppError::NotFound("List not found".into()));
    }
    sqlx::query("DELETE FROM list_leads WHERE list_id = $1 AND lead_id = $2")
        .bind(list_id)
        .bind(lead_id)
        .execute(&state.pool)
        .await?;
    Ok(Json(
        json!({ "removed": true, "list_id": list_id, "lead_id": lead_id }),
    ))
}

/// POST /api/v1/lists — create a new list. If `campaign_id` is set, the list is owned by
/// that campaign (the campaign creates its own fresh list named after it).
pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateListRequest>,
) -> Result<Json<List>, AppError> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO lists (id, tenant_id, name, campaign_id, description, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,NOW(),NOW())",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.name)
    .bind(req.campaign_id)
    .bind(&req.description)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, List>("SELECT * FROM lists WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

/// PUT /api/v1/lists/:id
pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateListRequest>,
) -> Result<Json<List>, AppError> {
    let owned: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM lists WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?;
    if owned.is_none() {
        return Err(AppError::NotFound("List not found".into()));
    }
    sqlx::query(
        "UPDATE lists SET name = COALESCE($2, name), description = COALESCE($3, description), updated_at = NOW() WHERE id = $1",
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.description)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, List>("SELECT * FROM lists WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

/// DELETE /api/v1/lists/:id
pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM lists WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("List not found".into()));
    }
    Ok(Json(json!({ "deleted": true, "id": id })))
}

// ---------------------------------------------------------------------------
// Helpers (used internally by campaigns / leads wiring)
// ---------------------------------------------------------------------------

/// Ensure a campaign owns a list. Called on campaign create: creates a fresh list named
/// after the campaign (campaign "Inbound Plumbing" -> list "Inbound Plumbing") if the
/// campaign doesn't already own one.
pub async fn ensure_campaign_list(
    state: &AppState,
    tenant_id: &Uuid,
    campaign_id: &Uuid,
    campaign_name: &str,
) -> Option<Uuid> {
    // Already owns one?
    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM lists WHERE campaign_id = $1 LIMIT 1")
            .bind(campaign_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
    if let Some(id) = existing {
        return Some(id);
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO lists (id, tenant_id, name, campaign_id, description, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,NOW(),NOW())",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(campaign_name)
    .bind(campaign_id)
    .bind(None as Option<&str>)
    .execute(&state.pool)
    .await
    .ok()?;
    Some(id)
}

/// Add a lead to a list if not already present (idempotent).
pub async fn add_lead_to_list(state: &AppState, list_id: &Uuid, lead_id: &Uuid) {
    let _ = sqlx::query(
        "INSERT INTO list_leads (list_id, lead_id) VALUES ($1, $2)
         ON CONFLICT (list_id, lead_id) DO NOTHING",
    )
    .bind(list_id)
    .bind(lead_id)
    .execute(&state.pool)
    .await;
}

/// All campaigns for a tenant that are linked to the given tag id
/// (metadata.coreswift.tag_id == tag_id). Returns (campaign_id, name).
pub async fn campaigns_for_tag(
    state: &AppState,
    tenant_id: &Uuid,
    tag_id: &str,
) -> Vec<(Uuid, String)> {
    sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, name FROM campaigns
         WHERE tenant_id = $1
           AND metadata -> 'coreswift' ->> 'tag_id' = $2",
    )
    .bind(tenant_id)
    .bind(tag_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default()
}
