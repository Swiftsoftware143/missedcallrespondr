use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::ticket::{
        CreateTicketMessageRequest, CreateTicketRequest, Ticket, TicketMessage, UpdateTicketRequest,
    },
    state::AppState,
};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub status: Option<String>,
    pub priority: Option<String>,
}

pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<Ticket>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = match (&q.status, &q.priority) {
        (Some(s), Some(p)) => sqlx::query_as::<_, Ticket>(
            "SELECT * FROM tickets WHERE tenant_id = $1 AND status = $2 AND priority = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
        )
        .bind(claims.aid).bind(s).bind(p).bind(limit).bind(offset).fetch_all(&state.pool).await?,
        (Some(s), None) => sqlx::query_as::<_, Ticket>(
            "SELECT * FROM tickets WHERE tenant_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid).bind(s).bind(limit).bind(offset).fetch_all(&state.pool).await?,
        (None, Some(p)) => sqlx::query_as::<_, Ticket>(
            "SELECT * FROM tickets WHERE tenant_id = $1 AND priority = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid).bind(p).bind(limit).bind(offset).fetch_all(&state.pool).await?,
        (None, None) => sqlx::query_as::<_, Ticket>(
            "SELECT * FROM tickets WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(claims.aid).bind(limit).bind(offset).fetch_all(&state.pool).await?,
    };
    Ok(Json(items))
}

pub async fn stats(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
        "SELECT \
            COUNT(*) FILTER (WHERE status='open'), \
            COUNT(*) FILTER (WHERE status='in_progress'), \
            COUNT(*) FILTER (WHERE status='resolved'), \
            COUNT(*) FILTER (WHERE status='closed'), \
            COUNT(*) \
         FROM tickets WHERE tenant_id = $1",
    )
    .bind(claims.aid)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(serde_json::json!({
        "open": row.0, "in_progress": row.1, "resolved": row.2, "closed": row.3, "total": row.4
    })))
}

pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateTicketRequest>,
) -> Result<Json<Ticket>, AppError> {
    crate::features::check_feature_limit(&state.pool, claims.aid, "max_tickets", "Tickets").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO tickets (id, tenant_id, subject, status, priority, assigned_to, contact_id, source, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.subject)
    .bind("open")
    .bind(req.priority.as_deref().unwrap_or("medium"))
    .bind(req.assigned_to)
    .bind(req.contact_id)
    .bind(req.source.as_deref().unwrap_or("manual"))
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;
    // First message = description body
    if let Some(desc) = req.description {
        if !desc.trim().is_empty() {
            let mid = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO ticket_messages (id, ticket_id, sender_type, sender_id, body, created_at) VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(mid).bind(id).bind("agent").bind(None::<Uuid>).bind(&desc).bind(now)
            .execute(&state.pool).await?;
        }
    }
    let item = sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let ticket =
        sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Ticket not found".into()))?;
    let messages = sqlx::query_as::<_, TicketMessage>(
        "SELECT * FROM ticket_messages WHERE ticket_id = $1 ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(
        serde_json::json!({ "ticket": ticket, "messages": messages }),
    ))
}

pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTicketRequest>,
) -> Result<Json<Ticket>, AppError> {
    let existing =
        sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Ticket not found".into()))?;
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE tickets SET subject=$1, status=$2, priority=$3, assigned_to=$4, contact_id=$5, updated_at=$6 WHERE id=$7",
    )
    .bind(req.subject.as_ref().unwrap_or(&existing.subject))
    .bind(req.status.as_ref().unwrap_or(&existing.status))
    .bind(req.priority.as_ref().unwrap_or(&existing.priority))
    .bind(req.assigned_to.or(existing.assigned_to))
    .bind(req.contact_id.or(existing.contact_id))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn add_message(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateTicketMessageRequest>,
) -> Result<Json<TicketMessage>, AppError> {
    let _existing =
        sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Ticket not found".into()))?;
    let mid = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO ticket_messages (id, ticket_id, sender_type, sender_id, body, created_at) VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(mid)
    .bind(id)
    .bind(req.sender_type.as_deref().unwrap_or("agent"))
    .bind(req.sender_id)
    .bind(&req.body)
    .bind(now)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, TicketMessage>("SELECT * FROM ticket_messages WHERE id = $1")
        .bind(mid)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM tickets WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Ticket not found".into()));
    }
    Ok(Json(serde_json::json!({"deleted": true, "id": id})))
}
