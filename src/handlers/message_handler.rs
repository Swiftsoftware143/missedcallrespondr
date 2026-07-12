use axum::{
    extract::{Extension, Path, State},
    Json,
};
use uuid::Uuid;

use crate::features;
use crate::{
    config::Claims,
    error::AppError,
    models::message::{CreateMessageRequest, Message},
    state::AppState,
};

pub async fn list_messages(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<Message>>, AppError> {
    let items = sqlx::query_as::<_, Message>(
        "SELECT * FROM messages WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

pub async fn create_message(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateMessageRequest>,
) -> Result<Json<Message>, AppError> {
    let tenant_id: Uuid = claims.aid;
    features::enforce_feature_limit(&state.pool, tenant_id, "max_messages", "Messages").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO messages (id, call_id, direction, from_number, to_number, body, status, sent_at, tenant_id, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(req.call_id)
    .bind(&req.direction)
    .bind(&req.from_number)
    .bind(&req.to_number)
    .bind(&req.body)
    .bind("sent")
    .bind(now)
    .bind(claims.aid)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, Message>("SELECT * FROM messages WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

pub async fn get_message(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Message>, AppError> {
    let item = sqlx::query_as::<_, Message>(
        "SELECT * FROM messages WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Message not found".into()))?;
    Ok(Json(item))
}
