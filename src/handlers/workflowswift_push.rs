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
    let url = &state.workflowswift_url;
    if url.is_empty() {
        return;
    }

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

    match req.send().await
    {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.is_success() {
                tracing::info!("workflowswift push succeeded for contact {}", contact.id);
            } else {
                tracing::warn!(
                    "workflowswift push returned {status} for contact {}: {body}",
                    contact.id
                );
            }
        }
        Err(e) => {
            tracing::warn!("workflowswift push failed for contact {}: {e}", contact.id);
        }
    }
}
