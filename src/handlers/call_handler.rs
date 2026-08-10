use axum::{
    extract::{Extension, Path, State},
    Json,
};
use uuid::Uuid;

use crate::features;
use crate::{
    config::Claims,
    error::AppError,
    models::call::{CreateInboundCallRequest, InboundCall, UpdateInboundCallRequest},
    state::AppState,
};

pub async fn list_calls(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<InboundCall>>, AppError> {
    let calls = sqlx::query_as::<_, InboundCall>(
        "SELECT * FROM inbound_calls WHERE tenant_id = $1 ORDER BY call_time DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(calls))
}

pub async fn create_call(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateInboundCallRequest>,
) -> Result<Json<InboundCall>, AppError> {
    let tenant_id: Uuid = claims.aid;
    features::enforce_feature_limit(&state.pool, tenant_id, "max_calls", "Calls").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();
    let call_time = req.call_time.unwrap_or(now);
    let disposition = req.disposition.unwrap_or_else(|| "missed".into());

    sqlx::query(
        "INSERT INTO inbound_calls (id, caller_number, caller_name, called_number, call_time, duration, recording_url, voicemail_url, disposition, tenant_id, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(id)
    .bind(&req.caller_number)
    .bind(&req.caller_name)
    .bind(&req.called_number)
    .bind(call_time)
    .bind(req.duration)
    .bind(&req.recording_url)
    .bind(&req.voicemail_url)
    .bind(&disposition)
    .bind(claims.aid)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let call = sqlx::query_as::<_, InboundCall>("SELECT * FROM inbound_calls WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(call))
}

pub async fn get_call(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<InboundCall>, AppError> {
    let call = sqlx::query_as::<_, InboundCall>(
        "SELECT * FROM inbound_calls WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Call not found".into()))?;
    Ok(Json(call))
}

pub async fn update_call(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateInboundCallRequest>,
) -> Result<Json<InboundCall>, AppError> {
    let existing = sqlx::query_as::<_, InboundCall>(
        "SELECT * FROM inbound_calls WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Call not found".into()))?;

    let now = chrono::Utc::now().naive_utc();
    sqlx::query(
        "UPDATE inbound_calls SET caller_name=$1, duration=$2, recording_url=$3, voicemail_url=$4, disposition=$5, updated_at=$6 WHERE id=$7",
    )
    .bind(req.caller_name.unwrap_or(existing.caller_name.unwrap_or_default()))
    .bind(req.duration.or(existing.duration))
    .bind(req.recording_url.or(existing.recording_url))
    .bind(req.voicemail_url.or(existing.voicemail_url))
    .bind(req.disposition.unwrap_or(existing.disposition))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let call = sqlx::query_as::<_, InboundCall>("SELECT * FROM inbound_calls WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(call))
}

pub async fn delete_call(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM inbound_calls WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Call not found".into()));
    }
    Ok(Json(serde_json::json!({"message": "Call deleted"})))
}

pub async fn get_call_voicemail(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let vm = sqlx::query_as::<_, crate::models::voicemail::Voicemail>(
        "SELECT * FROM voicemails WHERE call_id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?;

    match vm {
        Some(v) => Ok(Json(serde_json::to_value(v).unwrap())),
        None => Err(AppError::NotFound("No voicemail for this call".into())),
    }
}

pub async fn respond_to_call(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let call = sqlx::query_as::<_, InboundCall>(
        "SELECT * FROM inbound_calls WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Call not found".into()))?;

    // Create follow-up for missed calls
    if call.disposition == "missed" {
        let follow_id = Uuid::new_v4();
        let now = chrono::Utc::now().naive_utc();
        sqlx::query(
            "INSERT INTO follow_ups (id, call_id, follow_type, scheduled_at, status, tenant_id, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(follow_id)
        .bind(id)
        .bind("call_back")
        .bind(now + chrono::Duration::hours(1))
        .bind("pending")
        .bind(claims.aid)
        .bind(now)
        .bind(now)
        .execute(&state.pool)
        .await?;
    }

    Ok(Json(serde_json::json!({"message": "Response queued"})))
}
