//! CoreSwift integration surface — per-user (Zapier-style) connection from MissedCall
//! Respondr into CoreSwift CRM. Mirrors the IncentiveSwift integration so the pattern is
//! identical across all top-of-funnel tools.
//!
//!   GET /api/v1/integrations/coreswift/status  → is the account connected?
//!   GET /api/v1/integrations/coreswift/lists   → proxy to CoreSwift /api/external/lists
//!                                                (for the "connect campaign to a CoreSwift
//!                                                 list" dropdown)
//!
//! The account connects by storing a personal CoreSwift API key (`csk_...`, created in
//! CoreSwift's Integration Center) via the existing `POST /api/v1/provider-keys`
//! with `provider = "coreswift"` (+ optional `base_url`).

use axum::extract::{Extension, State};
use axum::Json;
use serde_json::{json, Value};

use crate::{
    config::Claims,
    error::AppError,
    handlers::coreswift_external::{get_coreswift_connection, is_connected},
    state::AppState,
};

/// GET /api/v1/integrations/coreswift/status
pub async fn status(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let connected = is_connected(&state, &claims.aid).await;
    Ok(Json(json!({ "connected": connected })))
}

/// GET /api/v1/integrations/coreswift/lists
/// Proxies CoreSwift's GET /api/external/lists so the UI can render a dropdown of the
/// user's CoreSwift lists to attach a campaign to.
pub async fn lists(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let (api_key, base_url) = get_coreswift_connection(&state, &claims.aid)
        .await
        .ok_or_else(|| AppError::NotFound("CoreSwift is not connected".to_string()))?;

    let url = format!("{base_url}/api/external/lists");
    let resp = reqwest::Client::new()
        .get(&url)
        .bearer_auth(&api_key)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("CoreSwift unreachable: {e}")))?;

    let status_code = resp.status();
    let body: Value = resp.json().await.unwrap_or_else(|_| json!({ "lists": [] }));

    if !status_code.is_success() {
        return Err(AppError::Internal(format!(
            "CoreSwift returned {status_code}: {}",
            serde_json::to_string(&body).unwrap_or_default()
        )));
    }

    Ok(Json(body))
}
