//! CoreSwift external push — per-user (personal API key) delivery of captured leads
//! from MissedCall Respondr into CoreSwift CRM.
//!
//! Replaces the old hardcoded `coreswift_push.rs` anti-pattern (hardcoded tenant UUID
//! plus global X-Internal-Key). Uses the per-account personal API key (stored in
//! `provider_keys` with provider="coreswift") and pushes to CoreSwift's
//! `/api/external/contacts` endpoint. This is the same Zapier-style deep-layer
//! integration that IncentiveSwift uses, so the pattern is identical across all
//! top-of-funnel tools.
//!
//! Per-campaign wiring:
//!   - A campaign can be connected to one CoreSwift list (stored in `campaigns.metadata`
//!     under `metadata.coreswift.list_id`).
//!   - A campaign can be linked to one MissedCall tag (`metadata.coreswift.tag_id`), so
//!     leads carrying that tag flow into the campaign's list.
//!   - When a lead is captured (or a lead's tags change) it is pushed to CoreSwift with
//!     its tags plus the connected list_id. Tag names propagate to the CoreSwift contact
//!     so everything stays neatly organized.
//!
use crate::state::AppState;
use serde_json::{json, Map, Value};
use uuid::Uuid;

/// Resolve the account's CoreSwift connection (personal API key + base URL) from
/// `provider_keys` (provider="coreswift"). Returns None if not connected.
pub async fn get_coreswift_connection(
    state: &AppState,
    account_id: &Uuid,
) -> Option<(String, String)> {
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT api_key, base_url FROM provider_keys
         WHERE tenant_id = $1 AND provider = 'coreswift' AND is_active = true",
    )
    .bind(account_id)
    .fetch_optional(&state.pool)
    .await
    .ok()??;

    let api_key = row.0;
    let base_url = row
        .1
        .filter(|u| !u.is_empty())
        .or_else(|| {
            let def = state.coreswift_url.trim().to_string();
            if def.is_empty() {
                None
            } else {
                Some(def)
            }
        })
        .map(|u| u.trim_end_matches('/').to_string())?;

    Some((api_key, base_url))
}

/// Is this account connected to CoreSwift?
pub async fn is_connected(state: &AppState, account_id: &Uuid) -> bool {
    get_coreswift_connection(state, account_id).await.is_some()
}

/// Fetch the campaign's connected CoreSwift list id from `metadata.coreswift.list_id`.
#[allow(dead_code)] // consumed by per-campaign list wiring (Increment B)
pub async fn get_campaign_coreswift_list(state: &AppState, campaign_id: &Uuid) -> Option<String> {
    let meta: Option<Value> = sqlx::query_scalar::<_, Option<Value>>(
        "SELECT metadata FROM campaigns WHERE id = $1 AND metadata IS NOT NULL",
    )
    .bind(campaign_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()?;

    meta.as_ref()
        .and_then(|m| m.get("coreswift"))
        .and_then(|c| c.get("list_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Fetch the campaign's linked tag id from `metadata.coreswift.tag_id`.
#[allow(dead_code)] // consumed by per-campaign list wiring (Increment B)
pub async fn get_campaign_tag_id(state: &AppState, campaign_id: &Uuid) -> Option<String> {
    let meta: Option<Value> = sqlx::query_scalar::<_, Option<Value>>(
        "SELECT metadata FROM campaigns WHERE id = $1 AND metadata IS NOT NULL",
    )
    .bind(campaign_id)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()?;

    meta.as_ref()
        .and_then(|m| m.get("coreswift"))
        .and_then(|c| c.get("tag_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Push a captured lead into CoreSwift via `/api/external/contacts`.
///
/// Args:
///   - contact/lead identity + contact info (name/company/email/phone)
///   - tags (MissedCall tag names, propagated to the CoreSwift contact)
///   - list_id (optional — the CoreSwift list this should land in)
///   - source (optional string like "missed-call" / "follow-up" / "manual")
///
/// Returns true on success. Never panics — failures are logged and swallowed so a
/// CoreSwift outage never blocks the entry/lead save.
#[allow(clippy::too_many_arguments)]
pub async fn push_lead_to_coreswift(
    state: &AppState,
    account_id: &Uuid,
    name: &str,
    company: Option<&str>,
    email: Option<&str>,
    phone: Option<&str>,
    tags: &[String],
    list_id: Option<&str>,
    source: Option<&str>,
    notes: Option<&str>,
) -> bool {
    let Some((api_key, base_url)) = get_coreswift_connection(state, account_id).await else {
        // Not connected to CoreSwift — nothing to do (this is the no-CRM user case).
        return false;
    };

    let mut body = Map::new();
    // Split name into first/last at first space for CoreSwift's fields.
    let name = name.trim();
    if !name.is_empty() {
        let mut parts = name.splitn(2, ' ');
        let first = parts.next().unwrap_or("").to_string();
        let last = parts.next().unwrap_or("").to_string();
        body.insert("first_name".into(), json!(first));
        if !last.is_empty() {
            body.insert("last_name".into(), json!(last));
        }
    }
    if let Some(e) = email.filter(|e| !e.trim().is_empty()) {
        body.insert("email".into(), json!(e.trim()));
    }
    if let Some(p) = phone.filter(|p| !p.trim().is_empty()) {
        body.insert("phone".into(), json!(p.trim()));
    }
    if let Some(c) = company.filter(|c| !c.trim().is_empty()) {
        body.insert("company".into(), json!(c.trim()));
    }
    body.insert(
        "source".into(),
        json!(source.unwrap_or("missedcallrespondr")),
    );
    body.insert("source_app".into(), json!("missedcallrespondr"));
    if let Some(lid) = list_id.filter(|l| !l.trim().is_empty()) {
        body.insert("list_id".into(), json!(lid.trim()));
    }
    if !tags.is_empty() {
        body.insert("tags".into(), json!(tags));
    }
    if let Some(n) = notes.filter(|n| !n.trim().is_empty()) {
        body.insert("notes".into(), json!(n.trim()));
    }

    let url = format!("{base_url}/api/external/contacts");

    let client = reqwest::Client::new();

    match client
        .post(&url)
        .bearer_auth(&api_key)
        .json(&Value::Object(body))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                tracing::info!(
                    "coreswift external push OK: lead {name} (list={})",
                    list_id.unwrap_or("-")
                );
                true
            } else {
                let b = resp.text().await.unwrap_or_default();
                tracing::warn!(
                    "coreswift external push returned {status} for lead {name}: {}",
                    b.chars().take(300).collect::<String>()
                );
                false
            }
        }
        Err(e) => {
            tracing::warn!("coreswift external push failed for lead {name}: {e}");
            false
        }
    }
}
