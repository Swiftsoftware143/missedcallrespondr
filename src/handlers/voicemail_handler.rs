use axum::{
    extract::{Extension, Path, State},
    Json,
};
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::voicemail::{UpdateVoicemailRequest, Voicemail},
    state::AppState,
};

pub async fn list_voicemails(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<Voicemail>>, AppError> {
    let items = sqlx::query_as::<_, Voicemail>(
        "SELECT * FROM voicemails WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

pub async fn get_voicemail(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Voicemail>, AppError> {
    let item = sqlx::query_as::<_, Voicemail>(
        "SELECT * FROM voicemails WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Voicemail not found".into()))?;
    Ok(Json(item))
}

pub async fn update_voicemail(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateVoicemailRequest>,
) -> Result<Json<Voicemail>, AppError> {
    let existing = sqlx::query_as::<_, Voicemail>(
        "SELECT * FROM voicemails WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Voicemail not found".into()))?;

    let now = chrono::Utc::now().naive_utc();
    sqlx::query(
        "UPDATE voicemails SET listened=$1, notes=$2, transcription=$3, updated_at=$4 WHERE id=$5",
    )
    .bind(req.listened.unwrap_or(existing.listened))
    .bind(req.notes.or(existing.notes))
    .bind(req.transcription.or(existing.transcription))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, Voicemail>("SELECT * FROM voicemails WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}
