use axum::{
    extract::{Extension, State},
    Json,
};

use crate::{config::Claims, error::AppError, models::call::InboundCall, state::AppState};

/// GET /api/v1/call-logs — the raw per-call billing log (`call_logs`, with cost/recorded).
/// The tenant console's Calls screen does not read this; it lists `inbound_calls` (GET /api/v1/calls).
/// Left as-is on purpose: the operator console's Ops panel and anything reading the billing log use it.
pub async fn list_call_logs(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::models::call_log::CallLog>>, AppError> {
    let items = sqlx::query_as::<_, crate::models::call_log::CallLog>(
        "SELECT * FROM call_logs WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

/// GET /api/v1/call-logs/export — the CSV behind the Calls screen's **Export** button.
///
/// kanban t_f06b1710 (wire-or-delete census). This route used to read `call_logs` while the console's
/// Calls screen renders `GET /api/v1/calls`, i.e. table `inbound_calls`. The two tables are written
/// 1:1 by the Telnyx webhook, but `inbound_calls` also accepts rows from `POST /api/v1/calls`, so a
/// button on that screen could have handed the operator a file that did not match the rows in front of
/// them. The export now reads the SAME table the screen lists and emits the SAME columns it renders
/// (Caller = the name when there is one, else the number — exactly the screen's rule), so "Export"
/// cannot diverge from what the operator sees.
pub async fn export_call_logs(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<String, AppError> {
    let items = sqlx::query_as::<_, InboundCall>(
        "SELECT * FROM inbound_calls WHERE tenant_id = $1 ORDER BY call_time DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;

    let mut csv = String::from("ID,Caller,Called,When,Duration Seconds,Disposition\n");
    for item in items {
        csv.push_str(&format!(
            "{},{},{},{},{},{}\n",
            item.id,
            csv_cell(item.caller_name.as_deref().unwrap_or(&item.caller_number)),
            csv_cell(&item.called_number),
            item.call_time,
            item.duration.map(|d| d.to_string()).unwrap_or_default(),
            csv_cell(&item.disposition),
        ));
    }

    Ok(csv)
}

/// A CSV cell carrying caller-supplied text: a name or a number can contain a comma, a quote or a
/// newline, and an unquoted one would shift every later column of that row.
fn csv_cell(v: &str) -> String {
    if v.contains(',') || v.contains('"') || v.contains('\n') || v.contains('\r') {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}
