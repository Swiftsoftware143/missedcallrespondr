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

/// Canonical capture-app slug this app reports to the hub (drives CoreSwift's
/// "leads by capture app" rollup; the hub normalizes aliases to this same value).
pub const SOURCE_APP: &str = "missedcallrespondr";

/// Base-URL resolution step 2 (fleet standard): the `coreswift` row in
/// `integration_provider_presets`. Step 1 is `provider_keys.base_url` (tenant override),
/// step 3 the `CORESWIFT_URL` env / constant default. Never hardcode-only.
async fn preset_base_url(state: &AppState) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT base_url FROM integration_provider_presets
         WHERE key = 'coreswift' AND is_active = true AND base_url <> ''",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .map(|u| u.trim_end_matches('/').to_string())
}

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

    // The column holds `enc:v1:` ciphertext at rest — decrypt before the key is used against
    // the hub. Rows written before encryption shipped are plaintext and pass through unchanged.
    let api_key =
        match crate::security::provider_key_crypto::decrypt_from_storage(&state.pool, &api_key)
            .await
        {
            Ok(k) => k,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    account = %account_id,
                    "coreswift: stored key could not be decrypted"
                );
                return None;
            }
        };

    // 1) tenant override → 2) preset row → 3) env/constant default.
    let base_url = match row.1.filter(|u| !u.is_empty()) {
        Some(url) => url,
        None => match preset_base_url(state).await {
            Some(url) => url,
            None => {
                let def = state.coreswift_url.trim().to_string();
                if def.is_empty() {
                    return None;
                }
                def
            }
        },
    }
    .trim_end_matches('/')
    .to_string();

    Some((api_key, base_url))
}

/// Live probe used by the Integration Center "Test connection" button: an authed
/// `GET /api/external/lists` against the hub. Returns (status_code, body_snippet).
pub async fn hub_probe(state: &AppState, account_id: &Uuid) -> Result<(u16, String), String> {
    let (api_key, base_url) = get_coreswift_connection(state, account_id)
        .await
        .ok_or_else(|| "Not connected: store a CoreSwift key first".to_string())?;

    let resp = reqwest::Client::new()
        .get(format!("{base_url}/api/external/lists"))
        .bearer_auth(&api_key)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("CoreSwift unreachable at {base_url}: {e}"))?;

    let code = resp.status().as_u16();
    let body: String = resp
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect();

    Ok((code, body))
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
/// Returns the provider's own reason on failure (`Err`) and `()` on success. Never panics.
/// Callers that must not block on a CRM outage use the bool wrapper
/// [`push_lead_to_coreswift`] below; the operator-facing "Push now" button uses THIS form so the
/// reason reaches the screen behind a 424.
#[allow(clippy::too_many_arguments)]
pub async fn push_lead_to_coreswift_detailed(
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
) -> Result<(), String> {
    // Harness residue never reaches the hub (see the `test-mail-hygiene` skill): a probe, canary or
    // test-run address is not a person, and the contact it creates in the operator's CRM outlives
    // the probe row that produced it. Measured 2026-10-20 in this fleet: a reserved address pushed
    // through this path answered `pushed: true` and POSTed to `/api/external/contacts` — exactly the
    // residue FunnelSwift and ADASwift already refuse. The predicate is the app's own reserved-domain
    // rule (the same one the mail seam uses), so a routable-but-typo'd address is still pushed.
    if let Some(addr) = email.map(str::trim).filter(|a| !a.is_empty()) {
        if crate::security::email_addr::is_reserved_address(addr) {
            tracing::info!(
                "CoreSwift push skipped: {addr} is a reserved test address (harness residue stays out of the hub)"
            );
            return Err(format!(
                "{addr} is a reserved test address, so no CRM contact was created"
            ));
        }
    }

    let Some((api_key, base_url)) = get_coreswift_connection(state, account_id).await else {
        // Not connected to CoreSwift — nothing to do (this is the no-CRM user case).
        return Err("CoreSwift is not connected — save your csk_ key first".to_string());
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
    body.insert("source".into(), json!(source.unwrap_or(SOURCE_APP)));
    body.insert("source_app".into(), json!(SOURCE_APP));
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
                Ok(())
            } else {
                let b: String = resp.text().await.unwrap_or_default();
                let snippet: String = b.chars().take(300).collect();
                tracing::warn!(
                    "coreswift external push returned {status} for lead {name}: {snippet}"
                );
                Err(format!("CoreSwift returned {status}: {snippet}"))
            }
        }
        Err(e) => {
            tracing::warn!("coreswift external push failed for lead {name}: {e}");
            Err(format!("CoreSwift unreachable at {base_url}: {e}"))
        }
    }
}

/// The bool face of [`push_lead_to_coreswift_detailed`], for the AUTOMATIC capture paths
/// (call / contact / lead / SMS): a CoreSwift outage must never block the entry save, so they log
/// and carry on. The operator-facing manual push uses the detailed form.
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
    push_lead_to_coreswift_detailed(
        state, account_id, name, company, email, phone, tags, list_id, source, notes,
    )
    .await
    .is_ok()
}
