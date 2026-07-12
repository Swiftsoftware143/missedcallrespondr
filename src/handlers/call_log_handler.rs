use axum::{
    extract::{Extension, Query, State},
    Json,
};
use std::collections::HashMap;

use crate::{
    config::Claims,
    error::AppError,
    models::call_log::CallLog,
    state::AppState,
};

pub async fn list_call_logs(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<CallLog>>, AppError> {
    let items = sqlx::query_as::<_, CallLog>(
        "SELECT * FROM call_logs WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

pub async fn export_call_logs(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<String, AppError> {
    let _format = params.get("format").map(|s| s.as_str()).unwrap_or("csv");

    let items = sqlx::query_as::<_, CallLog>(
        "SELECT * FROM call_logs WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;

    let mut csv = String::from("ID,Caller Number,Called Number,Duration,Disposition,Cost,Recorded,Notes,Created At\n");
    for item in items {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            item.id,
            item.caller_number,
            item.called_number,
            item.duration.unwrap_or(0),
            item.disposition,
            item.cost.unwrap_or(0.0),
            item.recorded,
            item.notes.unwrap_or_default(),
            item.created_at,
        ));
    }

    Ok(csv)
}
