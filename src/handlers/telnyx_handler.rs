use axum::{
    extract::{Extension, Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::Claims;
use crate::error::AppError;
use crate::state::AppState;

type ApiResult<T> = Result<T, AppError>;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct PhoneNumber {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub number: String,
    pub friendly_name: Option<String>,
    pub provider: String,
    pub is_active: bool,
    pub telnyx_connection_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct TelnyxConfig {
    pub id: Uuid,
    pub api_key: String,
    pub profile_id: Option<String>,
    pub messaging_profile_id: Option<String>,
    pub webhook_secret: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TelnyxWebhookPayload {
    pub data: Option<TelnyxWebhookData>,
    pub meta: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TelnyxWebhookData {
    pub event_type: Option<String>,
    pub id: Option<String>,
    pub occurred_at: Option<String>,
    pub payload: Option<TelnyxWebhookEventPayload>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct TelnyxWebhookEventPayload {
    pub call_control_id: Option<String>,
    pub connection_id: Option<String>,
    pub call_leg_id: Option<String>,
    pub call_session_id: Option<String>,
    pub client_state: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub direction: Option<String>,
    pub state: Option<String>,
    pub start_time: Option<String>,
    pub sip_source_ip: Option<String>,
    #[serde(default)]
    pub digits: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelnyxConfigUpdate {
    pub api_key: String,
    pub profile_id: Option<String>,
    pub messaging_profile_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PurchaseNumberRequest {
    pub number: String,
    #[serde(default)]
    pub friendly_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Client-string bounds (kanban t_c30d5d52)
// ---------------------------------------------------------------------------

/// Longest value the client may send for each client-supplied string that reaches a BOUNDED
/// column, taken from that column's own declaration:
/// `phone_numbers.number VARCHAR(32)`, `phone_numbers.friendly_name VARCHAR(255)`,
/// `telnyx_config.profile_id` / `telnyx_config.messaging_profile_id VARCHAR(255)`.
///
/// PostgreSQL counts CHARACTERS for `VARCHAR(n)` (`pg_column_size` is bytes, the constraint is
/// not), so the checks below count chars — a multi-byte `friendly_name` must not be refused early.
const NUMBER_MAX_CHARS: usize = 32;
const FRIENDLY_NAME_MAX_CHARS: usize = 255;
const TELNYX_PROFILE_ID_MAX_CHARS: usize = 255;

/// Refuse a client-supplied string longer than the column it is bound to with a 400 naming the
/// field and the limit.
///
/// Measured live on 2c96e12e before this existed: `POST /api/v1/telnyx/numbers` with a 400-char
/// `friendly_name` (and, separately, a 42-char `number`) reached the INSERT unvalidated and the
/// driver answered, so the caller got `500 {"error":"Database error"}` for a client-side typo
/// (the pre-t_4c15d597 build echoed `value too long for type character varying(255)` too). A
/// bounded column is a limit on the FIELD, and a request that breaks it is the caller's error:
/// 400, naming the field, before any provider call, plan gate or statement is paid for. Truncating
/// silently is the third option and is deliberately not taken — the client would never learn that
/// the name it typed is not the name stored.
fn check_len(field: &str, value: &str, max_chars: usize) -> Result<(), AppError> {
    let len = value.chars().count();
    if len > max_chars {
        return Err(AppError::BadRequest(format!(
            "{} is too long: {} characters, the maximum is {}",
            field, len, max_chars
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Look up tenant_id by called_number (the number Telnyx dialed in to).
async fn tenant_id_for_number(pool: &sqlx::PgPool, called_number: &str) -> Result<Uuid, AppError> {
    let tenant_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT tenant_id FROM phone_numbers WHERE number = $1 AND is_active = true LIMIT 1",
    )
    .bind(called_number)
    .fetch_optional(pool)
    .await?
    .flatten();

    tenant_id
        .ok_or_else(|| AppError::NotFound(format!("No tenant found for number: {}", called_number)))
}

/// Deduct one credit from the tenant; returns `false` if balance <= 0 after deduction.
async fn deduct_credit(pool: &sqlx::PgPool, tenant_id: Uuid) -> Result<bool, AppError> {
    let result = sqlx::query_scalar::<_, Option<i32>>(
        "UPDATE tenant_plans
         SET credit_balance = GREATEST(credit_balance - 1, 0),
             lifetime_credits = lifetime_credits + 1,
             updated_at = NOW()
         WHERE tenant_id = $1
         RETURNING credit_balance",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await?;

    match result {
        Some(Some(balance)) => Ok(balance > 0),
        _ => {
            // No plan row exists or NULL — treat as insufficient credits
            Ok(false)
        }
    }
}

/// Check if the tenant has their own (BYOK) Telnyx API key.
async fn tenant_has_own_telnyx(pool: &sqlx::PgPool, tenant_id: Uuid) -> Result<bool, AppError> {
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM provider_keys WHERE tenant_id = $1 AND provider = 'telnyx' AND is_active = true"
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

/// Fetch our global Telnyx config (single row).
///
/// `pub(crate)` because the outbound SMS transport reads the same credential from its own handler
/// (kanban t_2ed95642): one reader, one row, one place to change if the storage moves.
pub(crate) async fn get_telnyx_config(
    pool: &sqlx::PgPool,
) -> Result<Option<TelnyxConfig>, AppError> {
    let config = sqlx::query_as::<_, TelnyxConfig>("SELECT * FROM telnyx_config LIMIT 1")
        .fetch_optional(pool)
        .await?;
    Ok(config)
}

/// Apply one Telnyx MESSAGE event to the row the send path stored (kanban t_2ed95642).
///
/// Telnyx reports an outbound message twice: `message.sent` when the carrier takes it and
/// `message.finalized` when the recipient's network reports the outcome. Both carry the same MDR,
/// keyed by the provider's own message id, which is why the send path persists
/// `messages.provider_message_id`.
///
/// Rules, all of them the provider's own words:
///
/// * `to[0].status == "delivered"` (or an errors-free finalized event that says so) sets
///   `delivered_at`, and `sent_at` too if it was still NULL — a delivery implies the hand-off.
/// * `sent` moves the row to `sent` and fills `sent_at`.
/// * a failure — a failure word or a non-empty `errors` array — moves the row to `failed`.
/// * a `delivered` row is TERMINAL: no later event may un-confirm a delivery.
/// * an event whose id matches no row (every `message.received`, which this app does not store, and
///   any event for a row written before the transport existed) changes nothing and is acked.
///
/// The endpoint is public (Telnyx sends unauthenticated requests), so the answer says only whether a
/// row matched — never the row's contents.
async fn handle_message_event(
    state: &AppState,
    event_type: &str,
    body: &Value,
) -> ApiResult<Json<Value>> {
    let provider_id = body
        .pointer("/data/payload/id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let Some(provider_id) = provider_id else {
        tracing::info!(
            "Telnyx {} event carried no message id; nothing to match",
            event_type
        );
        return Ok(Json(json!({ "received": true, "matched": false })));
    };

    let word = body
        .pointer("/data/payload/to/0/status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let errors_present = body
        .pointer("/data/payload/errors")
        .and_then(|v| v.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    let mapped = match crate::handlers::message_handler::map_provider_status(word) {
        Some(("queued", _, _)) | None => None,
        Some((status, _, _)) => Some(status),
    };
    // A failed delivery is reported both ways; either one is the provider's verdict.
    let mapped = if errors_present {
        Some("failed")
    } else {
        mapped
    };

    let affected = match mapped {
        Some("delivered") => {
            let now = chrono::Utc::now().naive_utc();
            sqlx::query(
                "UPDATE messages
                 SET status = 'delivered', sent_at = COALESCE(sent_at, $2), delivered_at = COALESCE(delivered_at, $2)
                 WHERE provider_message_id = $1",
            )
            .bind(&provider_id)
            .bind(now)
            .execute(&state.pool)
            .await?
            .rows_affected()
        }
        Some("sent") => {
            let now = chrono::Utc::now().naive_utc();
            sqlx::query(
                "UPDATE messages
                 SET status = 'sent', sent_at = COALESCE(sent_at, $2)
                 WHERE provider_message_id = $1 AND status <> 'delivered'",
            )
            .bind(&provider_id)
            .bind(now)
            .execute(&state.pool)
            .await?
            .rows_affected()
        }
        Some("failed") => sqlx::query(
            "UPDATE messages SET status = 'failed'
             WHERE provider_message_id = $1 AND status <> 'delivered'",
        )
        .bind(&provider_id)
        .execute(&state.pool)
        .await?
        .rows_affected(),
        _ => 0,
    };

    tracing::info!(
        "Telnyx {} for message {} -> {} (rows affected: {})",
        event_type,
        provider_id,
        mapped.unwrap_or("no change"),
        affected
    );

    Ok(Json(json!({
        "received": true,
        "event": event_type,
        "matched": affected > 0,
    })))
}

/// Build a Telnyx call-control "hangup" response (JSON).
fn hangup_response() -> Json<Value> {
    Json(json!({
        "commands": [{
            "type": "hangup"
        }]
    }))
}

/// Build a Telnyx call-control "answer + record + gather" response.
fn answer_and_gather_response() -> Json<Value> {
    Json(json!({
        "commands": [
            {
                "type": "answer"
            },
            {
                "type": "record_start",
                "options": {
                    "format": "wav",
                    "play_beep": false
                }
            },
            {
                "type": "gather_using_audio",
                "options": {
                    "invalid_audio_url": "default",
                    "inter_digit_timeout_ms": 2000,
                    "max_digits": 1,
                    "timeout_millis": 10000
                }
            }
        ]
    }))
}

// ---------------------------------------------------------------------------
// 1. POST /api/v1/telnyx/webhook — Inbound webhook receiver (public route)
// ---------------------------------------------------------------------------
pub async fn webhook(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    // -- 1. Parse the Telnyx event
    let event_type = body
        .pointer("/data/event_type")
        .or_else(|| body.pointer("/data/event_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let call_control_id = body
        .pointer("/data/payload/call_control_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let from_number = body
        .pointer("/data/payload/from")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let to_number = body
        .pointer("/data/payload/to")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let _connection_id = body
        .pointer("/data/payload/connection_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    tracing::info!(
        "Telnyx webhook received: event_type={}, from={:?}, to={:?}, call_control_id={:?}",
        event_type,
        from_number,
        to_number,
        call_control_id
    );

    // -- 2. MESSAGE events (kanban t_2ed95642). The outbound send path stores the PROVIDER'S OWN
    //        message id (`messages.provider_message_id`), and the delivery events Telnyx sends
    //        afterwards are matched by it, so a row moves queued -> sent -> delivered (or failed)
    //        only when the provider says so. Handled before the call-control arm, which would
    //        otherwise ack them with empty commands and leave every row frozen at its send-time
    //        status — `delivered_at` NULL forever.
    if event_type.starts_with("message.") {
        return handle_message_event(&state, &event_type, &body).await;
    }

    // -- 3. Only process inbound calls
    match event_type.as_str() {
        "call_received" | "call_initiated" => { /* proceed */ }
        _ => {
            // Ack other events (answered, ringing, hangup, etc.) with empty commands
            return Ok(Json(json!({ "commands": [] })));
        }
    }

    // -- 3. Resolve tenant by the called number (the number the caller dialed)
    let called = to_number.clone().unwrap_or_default();

    // Normalize E.164
    let normalized_called = if called.starts_with('+') {
        called.clone()
    } else {
        format!("+{}", called)
    };

    let tenant_id = tenant_id_for_number(&state.pool, &normalized_called)
        .await
        .map_err(|_| {
            tracing::warn!("No tenant found for number {}", normalized_called);
            AppError::NotFound(format!("No tenant found for number: {}", normalized_called))
        })?;

    // -- 4. Check if tenant uses their own Telnyx key (BYOK) or our system
    let byok = tenant_has_own_telnyx(&state.pool, tenant_id).await?;

    if !byok {
        // -- 5. Deduct a credit
        let has_credits = deduct_credit(&state.pool, tenant_id).await?;
        if !has_credits {
            tracing::warn!(
                "Tenant {} has insufficient credits. Hanging up call from {}",
                tenant_id,
                from_number.as_deref().unwrap_or("unknown")
            );
            return Ok(hangup_response());
        }
    }

    // -- 6. Insert inbound_call record
    let caller = from_number.clone().unwrap_or_else(|| "unknown".to_string());
    let normalized_caller = if caller.starts_with('+') {
        caller.clone()
    } else {
        format!("+{}", caller)
    };

    let call_id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO inbound_calls (id, caller_number, caller_name, called_number, call_time, disposition, tenant_id, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
    )
    .bind(call_id)
    .bind(&normalized_caller)
    .bind(Option::<String>::None)  // caller_name
    .bind(&normalized_called)
    .bind(now)
    .bind("missed")
    .bind(tenant_id)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    // -- 7. Insert call_log record
    sqlx::query(
        "INSERT INTO call_logs (id, caller_number, called_number, duration, disposition, cost, recorded, tenant_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
    )
    .bind(Uuid::new_v4())
    .bind(&normalized_caller)
    .bind(&normalized_called)
    .bind(Option::<i32>::None)    // duration
    .bind("missed")
    .bind(if byok { None } else { Some(1.0) }) // cost (1 credit)
    .bind(false)                  // recorded
    .bind(tenant_id)
    .bind(now)
    .execute(&state.pool)
    .await?;

    // -- 7b. INBOUND capture → CoreSwift: the caller is a captured lead. This is the REAL
    //        missed-call capture path, so the push happens automatically — nobody presses a
    //        button. Best-effort in a spawned task: it never delays or fails the Telnyx
    //        call-control response, and it quietly no-ops when the tenant has not connected
    //        CoreSwift (BYOK). Shares the one CoreSwift code path
    //        (`coreswift_external::push_lead_to_coreswift`).
    {
        let st = state.clone();
        let lead_tenant = tenant_id;
        let lead_phone = normalized_caller.clone();
        let called = normalized_called.clone();
        let note = format!("Auto-captured by MissedCall Respondr: missed call to {called}");
        tokio::spawn(async move {
            crate::handlers::coreswift_external::push_lead_to_coreswift(
                &st,
                &lead_tenant,
                "",                        // no caller name on a raw inbound call
                None,                      // company
                None,                      // email (not available on a call)
                Some(lead_phone.as_str()), // phone
                &[],                       // tags
                None,                      // list (campaign wiring decides)
                Some("missed_call"),       // attribution
                Some(note.as_str()),       // notes
            )
            .await;
        });
    }

    // -- 8. Return Telnyx call-control commands (answer + gather)
    tracing::info!(
        "Processed Telnyx call for tenant {}: call_id={}",
        tenant_id,
        call_id
    );
    Ok(answer_and_gather_response())
}

// ---------------------------------------------------------------------------
// 2. GET /api/v1/telnyx/numbers — List phone numbers for current tenant
// ---------------------------------------------------------------------------
/// The tenant's HELD numbers — ACTIVE rows only (kanban t_4c15d597). A release is a soft delete
/// (`is_active = false`), so a released number used to stay in this list with no marker and the
/// console painted it green "active": the number was visible, offered as a sender, and unusable.
/// "Held" now means the same thing everywhere — `max_phone_numbers` counts ACTIVE rows
/// (t_b578b169), this list shows ACTIVE rows, and re-purchasing a released number revives it
/// (`purchase_number`). The released row stays in the table for history; it is simply no longer
/// part of the tenant's inventory. The admin console reads the same route, so a released number
/// leaves the panel listing the same way it leaves the tenant console.
pub async fn list_numbers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Vec<PhoneNumber>>> {
    let numbers = sqlx::query_as::<_, PhoneNumber>(
        "SELECT id, tenant_id, number, friendly_name, provider, is_active, telnyx_connection_id, created_at, updated_at
         FROM phone_numbers
         WHERE tenant_id = $1 AND is_active = true AND (provider = 'telnyx' OR telnyx_connection_id IS NOT NULL)
         ORDER BY number ASC"
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(numbers))
}

// ---------------------------------------------------------------------------
// 3. POST /api/v1/telnyx/numbers — Purchase/assign a new number
// ---------------------------------------------------------------------------
pub async fn purchase_number(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<PurchaseNumberRequest>,
) -> ApiResult<Json<Value>> {
    let tenant_id: Uuid = claims.aid;

    // Normalize number
    let number = if req.number.starts_with('+') {
        req.number.clone()
    } else {
        format!("+{}", req.number)
    };

    // INPUT VALIDATION FIRST (kanban t_c30d5d52): both client-supplied strings on this request are
    // bound to bounded columns and neither was checked, so a client-side mistake used to be answered
    // with a 500 from the driver. Measured live on 2c96e12e: `friendly_name` 400 chars ->
    // `500 {"error":"Database error"}` (the column is VARCHAR(255)) and `number` 42 chars -> 500
    // (VARCHAR(32)), while 255/32 stayed 200. Validation runs before the plan gate and before any
    // provider call — a malformed request is the caller's error whatever the tenant's plan says, and
    // nothing should be paid for first. `check_len` counts CHARACTERS, matching VARCHAR(n).
    check_len("number", &number, NUMBER_MAX_CHARS)?;
    if let Some(name) = req.friendly_name.as_deref() {
        check_len("friendly_name", name, FRIENDLY_NAME_MAX_CHARS)?;
    }

    // Plan gate FIRST, before the provider is called (kanban t_b578b169): `plans.features` sells
    // max_phone_numbers 1 (Free) / 5 (Pro Monthly) and the key was read by nothing. Gating here
    // means a tenant at its cap is refused BEFORE we ask Telnyx to buy a number they cannot hold.
    // The count is ACTIVE numbers only (see features::count_usage), matching the "already assigned"
    // check below — so releasing a number frees the slot.
    crate::features::enforce_feature_limit(
        &state.pool,
        tenant_id,
        "max_phone_numbers",
        "Phone numbers",
    )
    .await?;

    // One number = one row, GLOBALLY: `phone_numbers.number` carries the UNIQUE constraint
    // `phone_numbers_number_key` and a RELEASE is a soft delete (`is_active = false`, see
    // `delete_number` below), so the released row still owns the number. Looking the number up
    // WITHOUT an `is_active` filter is what makes "release then re-acquire the same number" a
    // supported operation: the previous predicate (`... AND is_active = true`) could not see the
    // released row, fell through to the INSERT, and the unique index answered 23505 — a 500 on a
    // documented path (kanban t_4c15d597). The released slot is intentionally reusable: the plan
    // gate above counts ACTIVE rows only (t_b578b169) and so does `list_numbers` below.
    // The lookup is GLOBAL by `number` on purpose (one number = one row), but an ACTIVE row owned by
    // another tenant is now REFUSED with 409 rather than reassigned (kanban t_e05013d4) — see the
    // arm below.
    let existing: Option<(Uuid, Uuid, bool)> =
        sqlx::query_as("SELECT id, tenant_id, is_active FROM phone_numbers WHERE number = $1")
            .bind(&number)
            .fetch_optional(&state.pool)
            .await?;

    if let Some((_existing_id, current_tenant, is_active)) = existing {
        if is_active {
            if current_tenant == tenant_id {
                return Err(AppError::Conflict(
                    "Number already assigned to your account".into(),
                ));
            }

            // ARM (a) — kanban t_e05013d4, decided by measurement: an ACTIVE number is NOT
            // transferable through this route. This route IS the tenant route (routes.rs puts
            // `POST /api/v1/telnyx/numbers` in the authenticated group with no role check) and
            // `PurchaseNumberRequest` carries no target tenant, so no caller can hand a number to
            // someone else — the old arm could only TAKE one. Measured live on 251193a4: tenant B
            // POSTing tenant A's active number answered `200 {"reassigned":true}`, flipped
            // `phone_numbers.tenant_id` to B and dropped the number out of A's own list. The
            // request's `reassigned` flag is read by no client (grepped: not in www-app, www-admin
            // or www), so nothing depends on the takeover.
            // A number another account holds is refused with 409 instead. The RELEASED arm below is
            // untouched: a released row is unowned inventory and stays claimable (t_4c15d597).
            return Err(AppError::Conflict(
                "This number is in use by another account".into(),
            ));
        }
    }

    // A RELEASED row (`is_active = false`) is revived below instead of re-inserted, so the freed
    // slot is genuinely usable by the tenant that released it (and by any tenant an admin hands the
    // number to). The provider leg still runs first for a non-BYOK tenant: `delete_number` released
    // the number at Telnyx too, so re-acquiring it has to re-buy it there; BYOK tenants manage that
    // side themselves.
    let released_id: Option<Uuid> = existing
        .as_ref()
        .filter(|(_, _, is_active)| !*is_active)
        .map(|(id, _, _)| *id);

    // Check if tenant has their own Telnyx key (BYOK) — if so, they manage
    // purchasing on their own Telnyx dashboard; we just register locally.
    let byok = tenant_has_own_telnyx(&state.pool, tenant_id).await?;

    if !byok {
        // Use our Telnyx config to purchase the number via API
        let telnyx_conf = get_telnyx_config(&state.pool)
            .await?
            .ok_or_else(|| AppError::Internal("Telnyx not configured by admin".into()))?;

        // Call Telnyx API to purchase the number
        let client = reqwest::Client::new();
        let resp = client
            .post("https://api.telnyx.com/v2/phone_numbers")
            .header("Authorization", format!("Bearer {}", telnyx_conf.api_key))
            .header("Content-Type", "application/json")
            .json(&json!({
                "phone_number": number,
                "connection_id": telnyx_conf.profile_id,
            }))
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("Telnyx API error: {}", e)))?;

        let resp_status = resp.status();
        let resp_body: Value = resp
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("Failed to parse Telnyx response: {}", e)))?;

        if !resp_status.is_success() {
            return Err(AppError::Internal(format!(
                "Telnyx purchase failed ({}): {}",
                resp_status, resp_body
            )));
        }
    }

    // Persist: REVIVE the released row when there is one (same row, same id, history kept), else
    // INSERT a fresh one. Neither arm can raise 23505 here: the lookup above is by the exact column
    // the unique index enforces, so at most one path runs per number.
    let id = match released_id {
        Some(id) => {
            sqlx::query(
                "UPDATE phone_numbers
                 SET tenant_id = $1, is_active = true, friendly_name = COALESCE($2, friendly_name), updated_at = NOW()
                 WHERE id = $3",
            )
            .bind(tenant_id)
            .bind(&req.friendly_name)
            .bind(id)
            .execute(&state.pool)
            .await?;

            tracing::info!(
                "phone_numbers: tenant {} re-acquired released number {} (row {})",
                tenant_id,
                number,
                id
            );

            id
        }
        None => {
            let id = Uuid::new_v4();
            let now = chrono::Utc::now();

            sqlx::query(
                "INSERT INTO phone_numbers (id, tenant_id, number, friendly_name, provider, is_active, created_at, updated_at)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
            )
            .bind(id)
            .bind(tenant_id)
            .bind(&number)
            .bind(&req.friendly_name)
            .bind("telnyx")
            .bind(true)
            .bind(now)
            .bind(now)
            .execute(&state.pool)
            .await?;

            id
        }
    };

    Ok(Json(json!({
        "id": id,
        "number": number,
        "assigned": true,
        "reassigned": false,
        "reactivated": released_id.is_some()
    })))
}

// ---------------------------------------------------------------------------
// 4. DELETE /api/v1/telnyx/numbers/:id — Release/unassign a number
// ---------------------------------------------------------------------------
pub async fn delete_number(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let tenant_id: Uuid = claims.aid;

    // Verify ownership
    let number = sqlx::query_as::<_, PhoneNumber>(
        "SELECT id, tenant_id, number, friendly_name, provider, is_active, telnyx_connection_id, created_at, updated_at
         FROM phone_numbers
         WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Phone number not found or not owned by tenant".into()))?;

    // If using our Telnyx (not BYOK), release via API
    let byok = tenant_has_own_telnyx(&state.pool, tenant_id).await?;
    if !byok {
        if let Some(conf) = get_telnyx_config(&state.pool).await? {
            let client = reqwest::Client::new();
            let _ = client
                .delete(format!(
                    "https://api.telnyx.com/v2/phone_numbers/{}",
                    number.number.trim_start_matches('+')
                ))
                .header("Authorization", format!("Bearer {}", conf.api_key))
                .send()
                .await;
        }
    }

    // Soft-delete (set inactive)
    sqlx::query("UPDATE phone_numbers SET is_active = false, updated_at = NOW() WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;

    Ok(Json(json!({
        "deleted": true,
        "id": id,
        "number": number.number
    })))
}

// ---------------------------------------------------------------------------
// 5. GET /api/v1/admin/telnyx-config — Get current Telnyx config (admin only)
// ---------------------------------------------------------------------------
pub async fn get_admin_config(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let config = get_telnyx_config(&state.pool).await?;

    match config {
        Some(c) => Ok(Json(json!({
            "id": c.id,
            "api_key": crate::handlers::provider_keys_handler::mask_key(&c.api_key),
            "profile_id": c.profile_id,
            "messaging_profile_id": c.messaging_profile_id,
            "has_webhook_secret": c.webhook_secret.is_some(),
        }))),
        None => Ok(Json(json!({
            "message": "Telnyx not configured"
        }))),
    }
}

// ---------------------------------------------------------------------------
// 6. PUT /api/v1/admin/telnyx-config — Update Telnyx config (admin only)
// ---------------------------------------------------------------------------
pub async fn put_admin_config(
    State(state): State<AppState>,
    Json(req): Json<TelnyxConfigUpdate>,
) -> ApiResult<Json<Value>> {
    let api_key = req.api_key;

    // Same class as the purchase route (kanban t_c30d5d52): `profile_id` and `messaging_profile_id`
    // are client-supplied strings bound to `telnyx_config` columns that are VARCHAR(255), and an
    // over-long one reached the UPDATE/INSERT unvalidated — measured live on 2c96e12e, a 300-char
    // `profile_id` answered `500 {"error":"Database error"}`. `api_key` is TEXT and deliberately
    // has no length check. Verified against information_schema before writing this.
    if let Some(pid) = req.profile_id.as_deref() {
        check_len("profile_id", pid, TELNYX_PROFILE_ID_MAX_CHARS)?;
    }
    if let Some(mpid) = req.messaging_profile_id.as_deref() {
        check_len("messaging_profile_id", mpid, TELNYX_PROFILE_ID_MAX_CHARS)?;
    }

    // Allow saving empty config — user will set keys later from Super Admin panel
    // if api_key.is_empty() {
    //     return Err(AppError::BadRequest("api_key is required".into()));
    // }

    let existing = sqlx::query_scalar::<_, Uuid>("SELECT id FROM telnyx_config LIMIT 1")
        .fetch_optional(&state.pool)
        .await?;

    match existing {
        Some(id) => {
            sqlx::query(
                "UPDATE telnyx_config SET api_key = $1, profile_id = COALESCE($2, profile_id), messaging_profile_id = COALESCE($3, messaging_profile_id), updated_at = NOW() WHERE id = $4"
            )
            .bind(&api_key)
            .bind(&req.profile_id)
            .bind(&req.messaging_profile_id)
            .bind(id)
            .execute(&state.pool)
            .await?;
        }
        None => {
            let id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO telnyx_config (id, api_key, profile_id, messaging_profile_id, created_at, updated_at) VALUES ($1, $2, $3, $4, NOW(), NOW())"
            )
            .bind(id)
            .bind(&api_key)
            .bind(&req.profile_id)
            .bind(&req.messaging_profile_id)
            .execute(&state.pool)
            .await?;
        }
    }

    Ok(Json(json!({
        "message": "Telnyx configuration updated",
        "api_key": crate::handlers::provider_keys_handler::mask_key(&api_key),
        "profile_id": req.profile_id,
        "messaging_profile_id": req.messaging_profile_id,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(field: &str, value: &str, max: usize) -> Option<String> {
        match check_len(field, value, max) {
            Err(AppError::BadRequest(msg)) => Some(msg),
            _ => None,
        }
    }

    /// The boundary the live probe measures (kanban t_c30d5d52): the limit itself is accepted, one
    /// character more is a 400 naming the field and the limit — never a silent truncation, never a
    /// 500 from the driver.
    #[test]
    fn the_limit_is_accepted_and_one_over_is_a_400_that_names_the_field() {
        assert!(check_len("friendly_name", &"y".repeat(255), 255).is_ok());
        let msg = refusal("friendly_name", &"z".repeat(256), 255).expect("256 must be refused");
        assert_eq!(
            msg,
            "friendly_name is too long: 256 characters, the maximum is 255"
        );
        assert!(
            msg.contains("friendly_name") && msg.contains("255"),
            "{msg}"
        );
    }

    /// `VARCHAR(n)` counts CHARACTERS, not bytes — a multi-byte name of legal length must pass, and
    /// the count in the message is a character count.
    #[test]
    fn the_check_counts_characters_not_bytes() {
        assert!(check_len("friendly_name", &"é".repeat(255), 255).is_ok());
        // 300 chars, 600 bytes: refused for the character count, not the byte count.
        let msg =
            refusal("friendly_name", &"é".repeat(300), 255).expect("300 chars must be refused");
        assert_eq!(
            msg,
            "friendly_name is too long: 300 characters, the maximum is 255"
        );
    }

    #[test]
    fn the_number_column_bound_is_32() {
        assert!(check_len("number", &format!("+{}", "5".repeat(31)), NUMBER_MAX_CHARS).is_ok());
        let msg = refusal("number", &format!("+{}", "5".repeat(32)), NUMBER_MAX_CHARS)
            .expect("33 chars must be refused");
        assert_eq!(msg, "number is too long: 33 characters, the maximum is 32");
    }
}
