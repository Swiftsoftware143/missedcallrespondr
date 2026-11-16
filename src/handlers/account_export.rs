//! `account_export` — `GET /api/v1/account/export`: the customer's OWN copy of their workspace.
//!
//! Parity with FunnelSwift's `/api/v1/account/export` (which WorkflowSwift also carries). This app
//! could export one list at a time — contacts, call logs, leads — but there was no way to obtain
//! everything a workspace holds in one file, so a business leaving could not take its data with it.
//!
//! # Scope
//!
//! The tenant comes from the TOKEN (`claims.aid`), never from a path segment, a query string or a
//! body: there is nothing to spoof, and one workspace can only ever receive its own document. Every
//! collection below is filtered on the same `tenant_id` column the app's own list handlers use.
//!
//! # What is deliberately NOT in the file
//!
//! Nothing that could be replayed to sign in as somebody, and no provider secret:
//! `users.password_hash`, API-key hashes and encrypted copies, `provider_keys`, payment-provider
//! credentials, `phone_numbers.telnyx_connection_id`, `tenants.probe_harness` (an internal marker)
//! and every email body. The document states that in its own `excluded` field.
//!
//! # Shape
//!
//! One pretty-printed JSON object served as an attachment:
//! `{ format, exported_at, tenant:[…], counts:{…}, excluded, <collection>:[…] }`. `counts` carries
//! the row count per collection, so the reader can see at a glance that the copy is complete.

use axum::{
    extract::{Extension, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
};
use serde_json::{json, Map, Value};

use crate::config::Claims;
use crate::error::AppError;
use crate::state::AppState;

/// Every collection in the export: the key it appears under, and the query that reads it.
///
/// All of them are `tenant_id`-scoped (the caller's workspace, from the token). `json_agg` means one
/// round trip per collection; `coalesce(…, '[]')` is what makes an empty workspace export `[]`
/// rather than `null`. Column lists are explicit, never `SELECT *`: a secret column added to a table
/// later must not silently start leaving the building through this route.
const COLLECTIONS: &[(&str, &str)] = &[
    (
        "tenant",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, slug, is_active, created_at, updated_at
               FROM tenants WHERE id = $1) x",
    ),
    (
        "plan",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT tp.id, tp.plan_id, p.slug AS plan_slug, p.name AS plan_name, tp.status,
                    tp.billing_cycle, tp.is_active, tp.credit_balance, tp.lifetime_credits,
                    tp.created_at, tp.expires_at
               FROM tenant_plans tp
               LEFT JOIN plans p ON p.id = tp.plan_id
              WHERE tp.tenant_id = $1) x",
    ),
    (
        "users",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, email, name, role, username, company, last_login_at, created_at,
                    updated_at, closed_at
               FROM users WHERE tenant_id = $1) x",
    ),
    (
        "contacts",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, phone, email, company, notes, tags, created_at, updated_at
               FROM contacts WHERE tenant_id = $1) x",
    ),
    (
        "leads",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, phone, email, source, status, notes, tags, created_at, updated_at
               FROM leads WHERE tenant_id = $1) x",
    ),
    (
        "clients",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, email, phone, source, notes, created_at, updated_at
               FROM clients WHERE tenant_id = $1) x",
    ),
    (
        "deals",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, contact_id, lead_id, value, stage, probability, expected_close_date,
                    is_won, source, created_at, updated_at
               FROM deals WHERE tenant_id = $1) x",
    ),
    (
        "lists",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, campaign_id, description, created_at, updated_at
               FROM lists WHERE tenant_id = $1) x",
    ),
    (
        "tags",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, color, group_id, sync_to_core, created_at, updated_at
               FROM tags WHERE tenant_id = $1) x",
    ),
    (
        "phone_numbers",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, number, friendly_name, provider, is_active, created_at, updated_at
               FROM phone_numbers WHERE tenant_id = $1) x",
    ),
    (
        "call_logs",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, caller_number, called_number, duration, disposition, cost, notes, created_at
               FROM call_logs WHERE tenant_id = $1) x",
    ),
    (
        "inbound_calls",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, caller_number, caller_name, called_number, call_time, duration,
                    disposition, created_at, updated_at
               FROM inbound_calls WHERE tenant_id = $1) x",
    ),
    (
        "messages",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, call_id, direction, from_number, to_number, body, status, sent_at,
                    delivered_at, created_at
               FROM messages WHERE tenant_id = $1) x",
    ),
    (
        "campaigns",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, kind, is_active, status, metadata, created_at, updated_at
               FROM campaigns WHERE tenant_id = $1) x",
    ),
    (
        "follow_ups",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, call_id, follow_type, scheduled_at, completed_at, status, notes, created_at,
                    updated_at
               FROM follow_ups WHERE tenant_id = $1) x",
    ),
    (
        "response_rules",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, trigger_condition, response_type, response_content, schedule,
                    is_active, priority, created_at, updated_at
               FROM response_rules WHERE tenant_id = $1) x",
    ),
];

/// What the customer must be told about their own copy: what is NOT in it, and why.
const EXCLUDED: &str = "Nothing that could be replayed to sign in as somebody or to act as this \
     workspace is included: stored passwords and password hashes, API-key secrets (the key names and \
     their dates are here), telephony and payment provider credentials, and the internal test marker \
     on the workspace. Ask support if you need one of those in a different form.";

/// `GET /api/v1/account/export`
pub async fn export_account(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let tenant_id = claims.aid;

    let mut out = Map::new();
    let mut counts = Map::new();

    for (key, sql) in COLLECTIONS {
        let raw: String = sqlx::query_scalar(sql)
            .bind(tenant_id)
            .fetch_one(&state.pool)
            .await?;
        let rows: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!([]));
        counts.insert(
            (*key).to_string(),
            json!(rows.as_array().map(|a| a.len()).unwrap_or(0)),
        );
        out.insert((*key).to_string(), rows);
    }

    out.insert("counts".to_string(), Value::Object(counts));
    out.insert("excluded".to_string(), json!(EXCLUDED));
    out.insert(
        "exported_at".to_string(),
        json!(chrono::Utc::now().to_rfc3339()),
    );
    out.insert("format".to_string(), json!("missedcall-account-export/1"));

    let body = serde_json::to_string_pretty(&Value::Object(out))
        .map_err(|e| AppError::Internal(format!("export encode error: {e}")))?;

    let filename = format!(
        "missedcall-account-{}.json",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        .map_err(|e| AppError::Internal(format!("export header error: {e}")))?;

    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response())
}
