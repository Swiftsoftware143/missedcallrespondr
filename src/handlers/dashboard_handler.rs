use axum::{
    extract::{Extension, State},
    Json,
};

use crate::{
    config::Claims,
    error::AppError,
    models::{activity::ActivityLog, dashboard::DashboardResponse},
    state::AppState,
};

pub async fn get_dashboard_stats(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<DashboardResponse>, AppError> {
    let total_calls: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbound_calls WHERE tenant_id = $1",
    )
    .bind(claims.aid)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);

    let missed_calls: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbound_calls WHERE tenant_id = $1 AND disposition = 'missed'",
    )
    .bind(claims.aid)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);

    let answered_calls: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM inbound_calls WHERE tenant_id = $1 AND disposition = 'answered'",
    )
    .bind(claims.aid)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);

    let voicemails: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM voicemails WHERE tenant_id = $1",
    )
    .bind(claims.aid)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);

    let follow_ups_pending: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM follow_ups WHERE tenant_id = $1 AND status = 'pending'",
    )
    .bind(claims.aid)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(0);

    let response_rate = if total_calls > 0 {
        (answered_calls as f64 / total_calls as f64) * 100.0
    } else {
        0.0
    };

    Ok(Json(DashboardResponse {
        total_calls,
        missed_calls,
        answered_calls,
        voicemails,
        follow_ups_pending,
        response_rate,
    }))
}

pub async fn get_dashboard_activity(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<ActivityLog>>, AppError> {
    let items = sqlx::query_as::<_, ActivityLog>(
        "SELECT * FROM activity_log WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT 50",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}
