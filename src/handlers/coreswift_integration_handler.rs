//! CoreSwift integration surface — per-user (Zapier-style) connection from MissedCall
//! Respondr into CoreSwift CRM. Mirrors the IncentiveSwift/WorkflowSwift integration so
//! the pattern is identical across all top-of-funnel tools.
//!
//!   GET  /api/v1/integrations/coreswift/status  → is the account connected? (+ base_url)
//!   GET  /api/v1/integrations/coreswift/lists   → proxy to CoreSwift /api/external/lists
//!                                                 (for the "connect campaign to a CoreSwift
//!                                                  list" dropdown)
//!   POST /api/v1/integrations/coreswift/push    → THE INBOUND PATH (manual fallback): proxy
//!                                                 hub POST /api/external/contacts
//!
//! The account connects by storing a personal CoreSwift API key (`csk_...`, created in
//! CoreSwift's Integration Center) via the existing `POST /api/v1/provider-keys`
//! with `provider = "coreswift"` (+ optional `base_url`).
//!
//! The automatic path — a captured call/caller pushed into CoreSwift with nobody pressing
//! a button — is `coreswift_external::push_lead_to_coreswift(...)`, called from the real
//! capture paths (`telnyx_handler::webhook`, `call_handler::create_call`,
//! `contact_handler`, `leads_handler`). One CoreSwift code path, not two.

use axum::extract::{Extension, Path, State};
use axum::Json;
use serde_json::{json, Value};

use crate::{
    config::Claims,
    error::AppError,
    handlers::coreswift_external::{
        get_coreswift_connection, hub_probe, is_connected, push_lead_to_coreswift, SOURCE_APP,
    },
    state::AppState,
};

/// GET /api/v1/integrations/coreswift/status
pub async fn status(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    // One resolver, one answer: same connection lookup the push path uses.
    let conn = get_coreswift_connection(&state, &claims.aid).await;
    let connected = conn.is_some();
    let base_url = conn.map(|(_, url)| url);

    Ok(Json(json!({
        "connected": connected,
        "base_url": base_url,
        "provider": "coreswift",
        "source_app": SOURCE_APP,
    })))
}

/// GET /api/v1/integrations/coreswift/lists
/// Proxies CoreSwift's GET /api/external/lists so the UI can render a dropdown of the
/// user's CoreSwift lists to attach a lead/campaign to.
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

/// Manual "push now" fallback body — every field is optional; the caller sends whatever
/// it captured. This is the same body shape the automatic capture paths build.
#[derive(Debug, Default, serde::Deserialize)]
pub struct PushLeadRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub company: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub list_id: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// POST /api/v1/integrations/coreswift/push — the inbound path.
///
/// Proxies CoreSwift's POST /api/external/contacts through the SAME helper the automatic
/// capture paths use. Not connected → a clean 400 (never a crash, never a silent success).
pub async fn push(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    body: Option<Json<PushLeadRequest>>,
) -> Result<Json<Value>, AppError> {
    let connected = is_connected(&state, &claims.aid).await;
    if !connected {
        return Err(AppError::BadRequest(
            "CoreSwift is not connected — store your csk_ key in the Integration Center first"
                .to_string(),
        ));
    }

    let req = body.map(|Json(b)| b).unwrap_or_default();
    let name = req.name.unwrap_or_default();
    let source = req
        .source
        .unwrap_or_else(|| SOURCE_APP.to_string())
        .trim()
        .to_string();

    let pushed = push_lead_to_coreswift(
        &state,
        &claims.aid,
        &name,
        req.company.as_deref(),
        req.email.as_deref(),
        req.phone.as_deref(),
        &req.tags,
        req.list_id.as_deref(),
        Some(source.as_str()),
        req.notes.as_deref(),
    )
    .await;

    if !pushed {
        return Err(AppError::Internal(
            "CoreSwift rejected the push — check the stored key and base URL".to_string(),
        ));
    }

    Ok(Json(json!({
        "status": "pushed",
        "pushed": true,
        "provider": "coreswift",
        "source": source,
        "source_app": SOURCE_APP,
    })))
}

/// POST /api/v1/provider-keys/:provider/test — live "Test connection" probe for the
/// Integration Center. Only CoreSwift has a meaningful probe (an authed hub call); other
/// providers report `ok: null` rather than pretending.
pub async fn test_provider_key(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, AppError> {
    if provider != "coreswift" {
        return Ok(Json(json!({
            "provider": provider,
            "ok": Value::Null,
            "message": "No live connection test available for this provider yet",
        })));
    }

    match hub_probe(&state, &claims.aid).await {
        Ok((code, body)) => Ok(Json(json!({
            "provider": "coreswift",
            "ok": code == 200,
            "status": code,
            "message": if code == 200 {
                "CoreSwift reachable — key accepted".to_string()
            } else {
                format!("CoreSwift returned {code}: {body}")
            },
        }))),
        Err(e) => Ok(Json(json!({
            "provider": "coreswift",
            "ok": false,
            "message": e,
        }))),
    }
}
