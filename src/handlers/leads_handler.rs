use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::lead::{CreateLeadRequest, Lead, UpdateLeadRequest},
    state::AppState,
};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub search: Option<String>,
    pub status: Option<String>,
}

pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Lead>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = if let (Some(status), Some(search)) = (&q.status, &q.search) {
        let pat = format!("%{}%", search);
        sqlx::query_as::<_, Lead>(
            "SELECT * FROM leads WHERE tenant_id = $1 AND status = $2 AND (name ILIKE $3 OR phone ILIKE $3 OR email ILIKE $3) ORDER BY created_at DESC LIMIT $4 OFFSET $5",
        )
        .bind(claims.aid)
        .bind(status)
        .bind(&pat)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else if let Some(status) = &q.status {
        sqlx::query_as::<_, Lead>(
            "SELECT * FROM leads WHERE tenant_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid)
        .bind(status)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else if let Some(search) = &q.search {
        let pat = format!("%{}%", search);
        sqlx::query_as::<_, Lead>(
            "SELECT * FROM leads WHERE tenant_id = $1 AND (name ILIKE $2 OR phone ILIKE $2 OR email ILIKE $2) ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid)
        .bind(&pat)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, Lead>(
            "SELECT * FROM leads WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
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
    Json(req): Json<CreateLeadRequest>,
) -> Result<Json<Lead>, AppError> {
    crate::features::enforce_feature_limit(&state.pool, claims.aid, "max_leads", "Leads").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO leads (id, tenant_id, name, phone, email, source, status, notes, tags, call_id, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.name)
    .bind(&req.phone)
    .bind(&req.email)
    .bind(req.source.as_deref().unwrap_or("call"))
    .bind(req.status.as_deref().unwrap_or("new"))
    .bind(&req.notes)
    .bind(&req.tags)
    .bind(req.call_id)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Lead>("SELECT * FROM leads WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;

    // Zapier-style CoreSwift push + campaign-list routing (best-effort, never blocks
    // the save). A lead carrying a tag that matches a campaign's linked tag is added to
    // that campaign's own list and pushed to that campaign's connected CoreSwift list.
    {
        let aid = claims.aid;
        let lead_id = item.id;
        let name = item.name.clone();
        let phone = item.phone.clone().unwrap_or_default();
        let email = item.email.clone().unwrap_or_default();
        let tags = item.tags.clone().unwrap_or_default();
        let notes = item.notes.clone().unwrap_or_default();
        let source = item.source.clone();
        let st = state.clone();

        // Resolve campaigns linked to any tag this lead carries.
        let matched: Vec<(Uuid, String)> = {
            let mut camps = Vec::new();
            for tag in &tags {
                let c = crate::handlers::lists_handler::campaigns_for_tag(&st, &aid, tag).await;
                camps.extend(c);
            }
            camps
        };

        tokio::spawn(async move {
            // 1) For each matched campaign: add lead to its own list + push to its
            //    CoreSwift list (if connected).
            for (campaign_id, campaign_name) in matched {
                if let Some(list_id) = crate::handlers::lists_handler::ensure_campaign_list(
                    &st,
                    &aid,
                    &campaign_id,
                    &campaign_name,
                )
                .await
                {
                    crate::handlers::lists_handler::add_lead_to_list(&st, &list_id, &lead_id).await;
                    // Push to the campaign's connected CoreSwift list.
                    let core_list =
                        crate::handlers::coreswift_external::get_campaign_coreswift_list(
                            &st,
                            &campaign_id,
                        )
                        .await;
                    crate::handlers::coreswift_external::push_lead_to_coreswift(
                        &st,
                        &aid,
                        &name,
                        None,
                        if email.is_empty() {
                            None
                        } else {
                            Some(email.as_str())
                        },
                        if phone.is_empty() {
                            None
                        } else {
                            Some(phone.as_str())
                        },
                        &tags,
                        core_list.as_deref(),
                        Some(source.as_str()),
                        if notes.is_empty() {
                            None
                        } else {
                            Some(notes.as_str())
                        },
                    )
                    .await;
                }
            }

            // 2) Standalone push (lead not in any matched campaign) — no list id, so it
            //    lands in the CoreSwift default/contact pool or the account's base flow.
            //    Avoid double-push if we already pushed to at least one campaign list.
            //    (Wired by the caller deciding whether to also do a standalone push.)
        });
    }

    Ok(Json(item))
}

pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Lead>, AppError> {
    let item = sqlx::query_as::<_, Lead>("SELECT * FROM leads WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Lead not found".into()))?;
    Ok(Json(item))
}

pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateLeadRequest>,
) -> Result<Json<Lead>, AppError> {
    let existing =
        sqlx::query_as::<_, Lead>("SELECT * FROM leads WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Lead not found".into()))?;
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE leads SET name=$1, phone=$2, email=$3, source=$4, status=$5, notes=$6, tags=$7, call_id=$8, updated_at=$9 WHERE id=$10",
    )
    .bind(req.name.as_ref().unwrap_or(&existing.name))
    .bind(req.phone.as_ref().or(existing.phone.as_ref()))
    .bind(req.email.as_ref().or(existing.email.as_ref()))
    .bind(req.source.as_ref().unwrap_or(&existing.source))
    .bind(req.status.as_ref().unwrap_or(&existing.status))
    .bind(req.notes.as_ref().or(existing.notes.as_ref()))
    .bind(req.tags.as_ref().or(existing.tags.as_ref()))
    .bind(req.call_id.or(existing.call_id))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Lead>("SELECT * FROM leads WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM leads WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Lead not found".into()));
    }
    Ok(Json(serde_json::json!({"deleted": true, "id": id})))
}
