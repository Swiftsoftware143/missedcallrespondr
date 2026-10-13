use crate::models::contact::Contact;
use crate::state::AppState;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct WorkflowSwiftRequest {
    source: String,
    campaign_slug: String,
    contact: ContactPayload,
    data: DataPayload,
    source_entry_id: String,
}

#[derive(Debug, Serialize)]
struct ContactPayload {
    first_name: String,
    last_name: String,
    email: String,
    phone: String,
    business_name: String,
}

#[derive(Debug, Serialize)]
struct DataPayload {
    contact: serde_json::Value,
}

/// Best-effort push of a newly created contact to WorkflowSwift.
/// This function never returns an error — failures are logged and swallowed.
pub async fn push_contact_to_workflowswift(state: &AppState, contact: &Contact) {
    // WorkflowSwift mounts its generic capture receiver at `/api/v1/incoming` (routes.rs:999,
    // nested under `/api/v1`). The path is appended HERE so `WORKFLOWSWIFT_URL` stays a plain base
    // URL, exactly as FunnelSwift's push does (FunnelSwift/src/handlers/workflowswift_push.rs:107).
    let base = state.workflowswift_url.trim_end_matches('/');
    if base.is_empty() {
        return;
    }
    let url = format!("{base}/api/v1/incoming");

    let contact_json = match serde_json::to_value(contact) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("workflowswift push: failed to serialize contact: {e}");
            return;
        }
    };

    let full_name = &contact.name;
    let parts: Vec<&str> = full_name.splitn(2, ' ').collect();
    let first_name = parts.first().unwrap_or(&"").to_string();
    let last_name = parts.get(1).unwrap_or(&"").to_string();

    let payload = WorkflowSwiftRequest {
        source: "missedcallrespondr".into(),
        campaign_slug: "missedcallrespondr".into(),
        contact: ContactPayload {
            first_name,
            last_name,
            email: contact.email.clone().unwrap_or_default(),
            phone: contact.phone.clone(),
            business_name: contact.company.clone().unwrap_or_default(),
        },
        data: DataPayload {
            contact: contact_json,
        },
        source_entry_id: contact.id.to_string(),
    };

    let client = reqwest::Client::new();
    let mut req = client
        .post(url)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(5));

    let internal_key = std::env::var("INTERNAL_SYNC_KEY").unwrap_or_default();
    if !internal_key.is_empty() {
        req = req.header("X-Internal-Key", &internal_key);
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let body: String = resp
                .text()
                .await
                .unwrap_or_default()
                .chars()
                .take(500)
                .collect();
            if !status.is_success() {
                tracing::warn!(
                    "workflowswift push returned {status} for contact {}: {body}",
                    contact.id
                );
                return;
            }
            // A 2xx is NOT proof that WorkflowSwift wrote anything. Its capture receiver answers
            // 200 `{"status":"accepted","matched":false,…}` when no active workflow matches the
            // source — the hand-off is accepted and NOTHING is created (incoming_handler.rs:109-117).
            // Logging "succeeded" for that reply is a false success: an operator grepping logs is
            // told a run exists when it does not. Mirror the FunnelSwift twin, which records the
            // same fix (kanban t_0a6a93f1): read `matched` and report the unmatched case
            // distinctly. Reporting only — the capture itself stays best-effort and never fails.
            let matched = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("matched").and_then(|m| m.as_bool()))
                .unwrap_or(false);
            if matched {
                tracing::info!("workflowswift push succeeded for contact {}", contact.id);
            } else {
                tracing::warn!(
                    "workflowswift push accepted but unmatched (no workflow for source \
                     'missedcallrespondr') for contact {}: {body}",
                    contact.id
                );
            }
        }
        Err(e) => {
            tracing::warn!("workflowswift push failed for contact {}: {e}", contact.id);
        }
    }
}
