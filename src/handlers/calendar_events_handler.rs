use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::calendar_event::{
        CalendarEvent, CreateCalendarEventRequest, UpdateCalendarEventRequest,
    },
    state::AppState,
};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
}

pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<CalendarEvent>>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let items = match (&q.from, &q.to) {
        (Some(from), Some(to)) => sqlx::query_as::<_, CalendarEvent>(
            "SELECT * FROM calendar_events WHERE tenant_id = $1 AND start_at >= $2 AND start_at <= $3 ORDER BY start_at ASC LIMIT $4 OFFSET $5",
        )
        .bind(claims.aid).bind(from).bind(to).bind(limit).bind(offset).fetch_all(&state.pool).await?,
        _ => sqlx::query_as::<_, CalendarEvent>(
            "SELECT * FROM calendar_events WHERE tenant_id = $1 ORDER BY start_at ASC LIMIT $2 OFFSET $3",
        )
        .bind(claims.aid).bind(limit).bind(offset).fetch_all(&state.pool).await?,
    };
    Ok(Json(items))
}

pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateCalendarEventRequest>,
) -> Result<Json<CalendarEvent>, AppError> {
    crate::features::check_feature_flag(&state.pool, claims.aid, "has_calendar", "Calendar")
        .await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO calendar_events (id, tenant_id, title, description, start_at, end_at, event_type, contact_id, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.title)
    .bind(&req.description)
    .bind(req.start_at)
    .bind(req.end_at)
    .bind(req.event_type.as_deref().unwrap_or("call"))
    .bind(req.contact_id)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, CalendarEvent>("SELECT * FROM calendar_events WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<CalendarEvent>, AppError> {
    let item = sqlx::query_as::<_, CalendarEvent>(
        "SELECT * FROM calendar_events WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Event not found".into()))?;
    Ok(Json(item))
}

pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateCalendarEventRequest>,
) -> Result<Json<CalendarEvent>, AppError> {
    let existing = sqlx::query_as::<_, CalendarEvent>(
        "SELECT * FROM calendar_events WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Event not found".into()))?;
    let now = chrono::Utc::now();
    sqlx::query(
        "UPDATE calendar_events SET title=$1, description=$2, start_at=$3, end_at=$4, event_type=$5, contact_id=$6, updated_at=$7 WHERE id=$8",
    )
    .bind(req.title.as_ref().unwrap_or(&existing.title))
    .bind(req.description.as_ref().or(existing.description.as_ref()))
    .bind(req.start_at.unwrap_or(existing.start_at))
    .bind(req.end_at.or(existing.end_at))
    .bind(req.event_type.as_ref().unwrap_or(&existing.event_type))
    .bind(req.contact_id.or(existing.contact_id))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, CalendarEvent>("SELECT * FROM calendar_events WHERE id = $1")
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
    let result = sqlx::query("DELETE FROM calendar_events WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Event not found".into()));
    }
    Ok(Json(serde_json::json!({"deleted": true, "id": id})))
}
