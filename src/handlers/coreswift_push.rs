use crate::models::contact::Contact;
use crate::state::AppState;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct TagSyncLead {
    id: String,
    name: String,
    email: String,
    company: Option<String>,
}

#[derive(Debug, Serialize)]
struct TagSyncRequest {
    source_app: String,
    tenant_id: String,
    lead: TagSyncLead,
    tags: Vec<String>,
    added_tags: Vec<String>,
    removed_tags: Vec<String>,
    triggered_by: String,
}

/// Best-effort push of a contact's tags to CoreSwift CRM via the cross-app tag-sync webhook.
/// This function never returns an error — failures are logged and swallowed.
pub async fn push_contact_to_coreswift(state: &AppState, contact: &Contact, triggered_by: &str) {
    let url = format!("{}/api/v1/webhooks/cross-app/tag-sync", state.coreswift_url);
    if state.coreswift_url.is_empty() {
        return;
    }

    let tags = contact.tags.clone().unwrap_or_default();

    let payload = TagSyncRequest {
        source_app: "missedcallrespondr".into(),
        tenant_id: contact.tenant_id.to_string(),
        lead: TagSyncLead {
            id: contact.id.to_string(),
            name: contact.name.clone(),
            email: contact.email.clone().unwrap_or_default(),
            company: contact.company.clone(),
        },
        tags,
        added_tags: vec![],
        removed_tags: vec![],
        triggered_by: triggered_by.to_string(),
    };

    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(5));

    let internal_key = std::env::var("INTERNAL_SYNC_KEY").unwrap_or_default();
    if !internal_key.is_empty() {
        req = req.header("x-internal-key", &internal_key);
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            if status.is_success() {
                tracing::info!(
                    "coreswift tag-sync succeeded for contact {} (triggered_by: {})",
                    contact.id,
                    triggered_by
                );
            } else {
                tracing::warn!(
                    "coreswift tag-sync returned {status} for contact {} (triggered_by: {}): {body}",
                    contact.id,
                    triggered_by
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                "coreswift tag-sync failed for contact {} (triggered_by: {}): {e}",
                contact.id,
                triggered_by
            );
        }
    }
}
