use axum::{
    body::Bytes,
    extract::{Extension, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::Claims;
use crate::error::AppError;
use crate::security::telnyx_signature;
use crate::state::AppState;
use crate::validation::check_len;

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
    /// The Telnyx ACCOUNT Ed25519 public key this deployment verifies inbound deliveries against
    /// (base64, 32 bytes), settable from the operator console (kanban t_38b352f8). Empty string
    /// means "not configured" — `TELNYX_PUBLIC_KEY` in the deploy env still wins when it is set.
    pub public_key: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

// The typed Telnyx webhook payload structs (`TelnyxWebhookPayload`, `TelnyxWebhookData`,
// `TelnyxWebhookEventPayload`) were removed with the gather arm (kanban t_6e679d39). Nothing ever
// read them — the receiver below parses the raw `Value` it is handed, which is what every
// `#[allow(dead_code)]` on those structs was hiding — and their one distinctive field, `digits`,
// existed only to feed a `call.gather.ended` consumer that was never written. A struct referenced
// only by its own definition is a promise; this app no longer makes it.

#[derive(Debug, Deserialize)]
pub struct TelnyxConfigUpdate {
    /// Optional from kanban t_38b352f8: the operator console now has a second credential on this
    /// form (the account signing key), and an operator may paste only that one. A blank value — or
    /// the masked value the console painted back — leaves the stored API key untouched.
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub profile_id: Option<String>,
    #[serde(default)]
    pub messaging_profile_id: Option<String>,
    /// The Telnyx ACCOUNT Ed25519 public key (base64, 32 bytes) from Mission Control. When absent
    /// or blank the stored key is kept, except that a deployment with no stored key and an API key
    /// saved here will have it read from Telnyx (`GET /v2/public_key`) on a best-effort basis.
    #[serde(default)]
    pub public_key: Option<String>,
    /// Explicit clear, so "leave the key alone" and "there is no key" are different requests
    /// (kanban t_38b352f8). Without it the console could SET a signing key but never remove one.
    #[serde(default)]
    pub clear_public_key: Option<bool>,
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

// `check_len` — the shared client-string length guard — now lives in `crate::validation` (kanban
// t_dd7be032 lifted it out of this module: the same class reaches far more than the telnyx route, so
// one helper serves every handler). The doc comment explaining WHY a bounded column is a 400 and not
// a truncated 200 moved with it, and this module's tests below still pin the boundary.

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

/// Take ONE credit for this call; `true` when the call may proceed.
///
/// Measured defect (2026-10-17): this used to decrement FIRST and then answer `balance > 0`, so a
/// tenant holding exactly one credit had it taken and was hung up on — fifty starter credits bought
/// forty-nine handled calls, and the refused call still incremented `lifetime_credits`. The
/// condition now lives in the UPDATE itself: a row is returned only when a credit was actually
/// there to spend, so "no credit" spends nothing (no balance move, no lifetime bump, no charge for
/// a call we refuse) and the LAST credit is spent on the last call it can pay for.
async fn deduct_credit(pool: &sqlx::PgPool, tenant_id: Uuid) -> Result<bool, AppError> {
    let result = sqlx::query_scalar::<_, Option<i32>>(
        "UPDATE tenant_plans
         SET credit_balance = credit_balance - 1,
             lifetime_credits = lifetime_credits + 1,
             updated_at = NOW()
         WHERE tenant_id = $1 AND credit_balance > 0
         RETURNING credit_balance",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await?;

    // A returned row means the UPDATE matched and applied; `None` means the tenant had no active
    // plan row OR no credit left, and in both cases nothing was written above.
    match result {
        Some(Some(_)) => Ok(true),
        _ => Ok(false),
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

// ---------------------------------------------------------------------------
// The inbound signing key — where the receiver gets it (kanban t_38b352f8)
// ---------------------------------------------------------------------------

/// Length of a raw Ed25519 public key, in bytes. The only length Telnyx issues.
const ED25519_PUBLIC_KEY_LEN: usize = 32;

/// Which credential the inbound receiver verifies a delivery against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigningKeySource {
    /// `TELNYX_PUBLIC_KEY` in the deploy environment. WINS whenever it is set.
    Env,
    /// `telnyx_config.public_key`, saved from the operator console's Telnyx panel.
    Config,
    /// Neither: the receiver refuses every delivery `503 telnyx_verification_not_configured`.
    None,
}

impl SigningKeySource {
    pub fn as_str(self) -> &'static str {
        match self {
            SigningKeySource::Env => "env",
            SigningKeySource::Config => "config",
            SigningKeySource::None => "none",
        }
    }
}

/// The key the inbound receiver must verify against, and where it came from.
///
/// ENV FIRST, then the operator row. A deployment that sets `TELNYX_PUBLIC_KEY` therefore behaves
/// exactly as before and — because the stored value is only read when the env carries none — pays
/// no database read on the delivery path at all. Both empty is the pre-existing posture: `None`,
/// which the receiver answers `503 telnyx_verification_not_configured`.
///
/// A database error is propagated rather than swallowed: refusing a delivery because the stored key
/// could not be read is failing CLOSED, and Telnyx retries a non-2xx delivery.
pub(crate) async fn resolve_signing_key(
    env_key: Option<&str>,
    pool: &sqlx::PgPool,
) -> Result<(Option<String>, SigningKeySource), AppError> {
    if let Some(key) = env_key.map(str::trim).filter(|k| !k.is_empty()) {
        return Ok((Some(key.to_string()), SigningKeySource::Env));
    }
    let stored: Option<String> = sqlx::query_scalar("SELECT public_key FROM telnyx_config LIMIT 1")
        .fetch_optional(pool)
        .await?;
    let stored = stored
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty());
    match stored {
        Some(key) => Ok((Some(key), SigningKeySource::Config)),
        None => Ok((None, SigningKeySource::None)),
    }
}

/// Validate an operator-supplied account signing key, returning the value to store.
///
/// Refused HERE rather than at the first delivery: a value that cannot be a 32-byte Ed25519 key
/// would leave the receiver answering `503 telnyx_public_key_invalid` on every call, which looks
/// like a Telnyx fault and is not. The sentence names the field, what was decoded, and where the
/// value comes from.
fn validate_public_key(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("public_key is empty".to_string());
    }
    let mut decoded_len: Option<usize> = None;
    for engine in [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE,
    ] {
        if let Ok(bytes) = engine.decode(trimmed) {
            if bytes.len() == ED25519_PUBLIC_KEY_LEN {
                return Ok(trimmed.to_string());
            }
            decoded_len = Some(bytes.len());
        }
    }
    Err(match decoded_len {
        Some(n) => format!(
            "public_key is not a Telnyx account signing key: it decodes to {n} bytes and must decode to \
             {ED25519_PUBLIC_KEY_LEN}. Copy the ACCOUNT public key from Telnyx Mission Control (base64, \
             32 bytes)."
        ),
        None => "public_key is not valid base64. Copy the ACCOUNT public key from Telnyx Mission \
                 Control (base64, 32 bytes)."
            .to_string(),
    })
}

/// Best-effort convenience arm: read the account's signing key with the account's OWN API key.
///
/// The supported path is the paste field above — Telnyx shows the account public key in Mission
/// Control and an operator can copy it. This arm exists so that saving the API key (which the
/// deployment needs anyway to buy numbers and send texts) can ALSO bring inbound verification up,
/// with no file edit and no restart. It is deliberately best-effort: every failure is reported to
/// the caller and changes nothing else, because a wrong guess here must never leave an operator
/// believing inbound is configured when it is not.
///
/// Telnyx answers `data.public_key` (a `data` array or a top-level `public_key` on some versions),
/// so all three shapes are read and the value is validated before it is stored.
async fn derive_public_key(api_base: &str, api_key: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| format!("could not build the HTTP client: {e}"))?;
    let answer = client
        .get(format!("{api_base}/v2/public_key"))
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|e| format!("could not reach Telnyx to read the account public key: {e}"))?;
    let status = answer.status().as_u16();
    let body: Value = answer.json().await.unwrap_or(Value::Null);
    let candidate = body
        .pointer("/data/public_key")
        .or_else(|| body.pointer("/data/0/public_key"))
        .or_else(|| body.pointer("/public_key"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if candidate.is_empty() {
        return Err(format!(
            "Telnyx answered HTTP {status} without a public key this app recognises; paste the account \
             public key from Telnyx Mission Control into the signing key field instead"
        ));
    }
    validate_public_key(candidate)
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

/// Build the Telnyx call-control reply for a handled inbound call: **answer only**.
///
/// The `gather_using_audio` command this used to carry is retired (kanban t_6e679d39), on
/// measurement rather than taste — four independent facts, each one checkable:
///
/// * **Nothing consumed the digits.** The receiver's `match event_type` handles only
///   `call_received` / `call_initiated`, so the `call.gather.ended` event Telnyx sends when a digit
///   is pressed was acked with `{"commands":[]}`. The Telnyx OpenAPI spec is explicit that this is
///   the event that carries them (`payload.digits`, `payload.status` ∈ valid|invalid|call_hangup|
///   cancelled|cancelled_amd|timeout), i.e. the digit reached no route, no task and no row.
/// * **There was nothing to press it for.** A gather with neither `audio_url` nor `media_name` plays
///   no prompt, so the caller hears silence and is never told to press anything; and no surface ever
///   promised DTMF. A census over the served roots (`www/`, `www-app/`, `www-admin/` in the repo and
///   under `/opt/swift/nginx/`) for gather|DTMF|IVR|keypad|press-a-digit returned 0 hits.
/// * **The command was not the provider's shape.** The spec has no `options` key at all (0
///   occurrences in the OpenAPI document), and the option names used here — `max_digits` (0
///   occurrences) and `inter_digit_timeout_ms` (0 occurrences) — are not the documented
///   `maximum_digits` / `inter_digit_timeout_millis`; `invalid_audio_url` was set to the literal
///   `"default"`, which is not a URL of a WAV or MP3 file.
/// * **The app has never taken a call.** `phone_numbers` = 0, `telnyx_config` = 0,
///   `inbound_calls` = 0 on live.
///
/// Wiring instead would have meant inventing a product surface — a prompt asset and a digit→action
/// contract — for a behaviour nothing advertises, and there is no `POST /api/v1/calls/:id/respond`
/// counterpart a caller could already reach. The `answer` command is KEPT, so the handled call flow
/// (the call is answered, one credit is deducted, `inbound_calls` / `call_logs` are written, and the
/// CoreSwift lead push and the response-rule evaluation are spawned) is exactly as before.
fn answer_response() -> Json<Value> {
    Json(json!({
        "commands": [{
            "type": "answer"
        }]
    }))
}

// ---------------------------------------------------------------------------
// 1. POST /api/v1/telnyx/webhook — Inbound webhook receiver (public route)
// ---------------------------------------------------------------------------

/// One log line per refused Telnyx delivery, at ERROR when the refusal is this deployment's own
/// (a missing/unreadable `TELNYX_PUBLIC_KEY` means inbound voice and delivery events cannot work at
/// all, and a silent receiver is the failure mode that started this card — t_0e4ae7b7).
fn log_telnyx_refusal(rejection: &telnyx_signature::TelnyxRejection) {
    if rejection.is_configuration() {
        tracing::error!(
            "Telnyx webhook REFUSED, nothing applied: {} — {}",
            rejection.reason,
            rejection.detail
        );
    } else {
        tracing::warn!(
            "Telnyx webhook refused: {} — {}",
            rejection.reason,
            rejection.detail
        );
    }
}

/// `POST /api/v1/telnyx/webhook` — the app's inbound telephony door.
///
/// The credential is the delivery's own Ed25519 signature (kanban t_0e4ae7b7): nothing here reads
/// or applies an event until [`telnyx_signature::verify`] has authenticated the raw bytes against
/// this deployment's `TELNYX_PUBLIC_KEY`, judged the `telnyx-timestamp` freshness window, and
/// confirmed the delivery is not a replay of one already processed. The extractor is `Bytes` —
/// never `Json` — because the signature covers the exact bytes Telnyx sent and re-serialising a
/// parsed `Value` would change them. A delivery that fails verification is answered `401` (`503`
/// when this deployment has no key configured at all) and nothing from it is applied.
///
/// The route stays on the anonymous allowlist: a webhook cannot present a JWT. The signature IS the
/// credential — see `crate::auth::route_policy::PUBLIC_ROUTES`.
pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Response> {
    let now = telnyx_signature::now_unix();
    let tolerance = state.config.telnyx_signature_tolerance_secs;
    // The account key: `TELNYX_PUBLIC_KEY` from the deploy env when set (byte-identical to the
    // pre-t_38b352f8 behaviour, and no database read), else the operator panel's row. Both empty
    // keeps the fail-closed 503 the receiver has always answered.
    let (signing_key, key_source) =
        resolve_signing_key(state.config.telnyx_public_key.as_deref(), &state.pool).await?;
    if key_source == SigningKeySource::Config {
        // Only reached when the deploy env carried no key: say which credential is in force, once
        // per delivery, so an operator reading a refusal never has to guess where the key came from.
        tracing::debug!(
            "Telnyx delivery verification key in force: operator console (config); the deploy env \
             carries no TELNYX_PUBLIC_KEY"
        );
    }
    let delivery =
        match telnyx_signature::verify(signing_key.as_deref(), &headers, &body, now, tolerance) {
            Ok(delivery) => delivery,
            Err(rejection) => {
                log_telnyx_refusal(&rejection);
                return Ok(rejection.into_response());
            }
        };
    if telnyx_signature::replay_guard().is_replayed(&delivery.signature, now, tolerance) {
        let rejection = telnyx_signature::replayed_rejection();
        log_telnyx_refusal(&rejection);
        return Ok(rejection.into_response());
    }

    // Only a verified delivery is parsed. An anonymous caller therefore never even gets a JSON
    // parse verdict out of this route.
    let payload: Value = serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON body: {e}")))?;
    let response = process_webhook_event(&state, &payload).await?;
    // Only now — with the effect landed, because any `?` above returned early — is the delivery
    // remembered, so a delivery whose first attempt failed on the database stays retryable.
    telnyx_signature::replay_guard().ack(&delivery.signature, now, tolerance);
    Ok(response.into_response())
}

/// The receiver's own work, run only after the delivery has been authenticated.
async fn process_webhook_event(state: &AppState, body: &Value) -> ApiResult<Json<Value>> {
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
        return handle_message_event(state, &event_type, body).await;
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
        // -- 5. Spend a credit, if one is left (see `deduct_credit`: the last credit pays for a call)
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

    // Activity feed (best-effort) — the customer's dashboard says a call landed. This is the REAL
    // intake path, so it is written here as well as in the manual/API one.
    crate::activity::call_received(
        &state.pool,
        tenant_id,
        call_id,
        &normalized_caller,
        "missed",
    )
    .await;

    // -- 7. Insert call_log record
    sqlx::query(
        // `recorded` was dropped from the table (kanban t_1d4fc956 RETIRE-VOICEMAILS): this writer
        // hardcoded `false` and no surface ever read the column, so it asserted a recording that
        // cannot exist. `notes` is untouched.
        "INSERT INTO call_logs (id, caller_number, called_number, duration, disposition, cost, tenant_id, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
    )
    .bind(Uuid::new_v4())
    .bind(&normalized_caller)
    .bind(&normalized_called)
    .bind(Option::<i32>::None)    // duration
    .bind("missed")
    .bind(if byok { None } else { Some(1.0) }) // cost (1 credit)
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
        let st = AppState::clone(state);
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

    // -- 7c. RESPONSE RULES (kanban t_31f9cf38). The tenant's ACTIVE rules are evaluated for this
    //        call: the first match by priority performs its action — a text back to the caller
    //        through the same Telnyx transport the Send Message form uses, or a callback queued in
    //        `follow_ups`. Spawned like 7b, so a slow provider call can never delay the call-control
    //        answer Telnyx is waiting for; what happened lands as a row in the tenant's Messages /
    //        Follow Ups log either way.
    {
        let st = AppState::clone(state);
        let rule_tenant = tenant_id;
        let rule_call = call_id;
        let rule_caller = normalized_caller.clone();
        let rule_called = normalized_called.clone();
        tokio::spawn(async move {
            crate::handlers::response_rule_eval::run_for_inbound_call(
                &st,
                rule_tenant,
                rule_call,
                &rule_caller,
                &rule_called,
            )
            .await;
        });
    }

    // -- 8. Return the Telnyx call-control reply: ANSWER ONLY (kanban t_6e679d39 — the gather arm
    //        is retired; see `answer_response` for the four measurements behind it).
    tracing::info!(
        "Processed Telnyx call for tenant {}: call_id={} (call-control reply: answer only; the gather arm is retired)",
        tenant_id,
        call_id
    );
    Ok(answer_response())
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
        let telnyx_conf = get_telnyx_config(&state.pool).await?.ok_or_else(|| {
            // Nothing is attempted when the OPERATOR has not connected a phone provider, so this is
            // not a server fault: the fleet's own rule for a missing operator configuration is 503
            // (see `AppError::ServiceUnavailable`, added for the outbound SMS path in kanban
            // t_2ed95642 — the same `telnyx_config` emptiness). This arm was MISSED by that change
            // and answered `500 {"error":"Telnyx not configured by admin"}` to the customer who
            // pressed "+ Add Number": measured live 2026-10-25 on a free probe workspace, and 500 is
            // both a false alarm in monitoring and operator jargon in the buyer's face. The sentence
            // below is what the dashboard prints after "Could not add the number: ", so it says what
            // happened, that nothing was charged, and what to do next.
            AppError::ServiceUnavailable(
                "Phone numbers can't be added yet — the phone service is not set up on this \
                 deployment. Nothing was charged. Please try again later or contact support."
                    .into(),
            )
        })?;

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
    // The SAME resolution the delivery path performs, so the panel can never disagree with the
    // refusal an operator is reading off a refused call.
    let (resolved, source) =
        resolve_signing_key(state.config.telnyx_public_key.as_deref(), &state.pool).await?;
    let ready = resolved.is_some();
    let note = if ready {
        format!(
            "Inbound calls and texts are verified against the Telnyx account signing key (source: {}).",
            match source {
                SigningKeySource::Env => "the deployment environment",
                SigningKeySource::Config => "the operator panel",
                SigningKeySource::None => "none",
            }
        )
    } else {
        "No Telnyx account signing key is set, so EVERY inbound call and text is refused \
         (503 telnyx_verification_not_configured). Save the Telnyx API key below and the app reads \
         the account key from Telnyx, or paste the account public key from Telnyx Mission Control \
         into the signing key field."
            .to_string()
    };

    match config {
        Some(c) => Ok(Json(json!({
            "id": c.id,
            "api_key": crate::handlers::provider_keys_handler::mask_key(&c.api_key),
            "profile_id": c.profile_id,
            "messaging_profile_id": c.messaging_profile_id,
            "has_webhook_secret": c.webhook_secret.is_some(),
            "has_public_key": ready,
            "public_key_source": source.as_str(),
            "public_key_set_in_panel": !c.public_key.trim().is_empty(),
            "inbound_verification_ready": ready,
            "inbound_verification_note": note,
        }))),
        None => Ok(Json(json!({
            "message": "Telnyx not configured",
            "has_public_key": ready,
            "public_key_source": source.as_str(),
            "inbound_verification_ready": ready,
            "inbound_verification_note": note,
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

    let existing = get_telnyx_config(&state.pool).await?;
    let stored_api_key = existing
        .as_ref()
        .map(|c| c.api_key.clone())
        .unwrap_or_default();
    let stored_public_key = existing
        .as_ref()
        .map(|c| c.public_key.clone())
        .unwrap_or_default();

    // Allow saving empty config — an operator will set keys later from the operator console.
    //
    // A blank API key, or the MASKED value the console painted back, means "leave the stored key
    // alone". Without that rule this form became a way to DESTROY the sending credential the moment
    // its second field (the signing key, kanban t_38b352f8) was used: the generic panel renders the
    // masked key it read, and whatever the box holds would have been posted back as the key.
    let submitted_api_key = req.api_key.trim().to_string();
    let api_key = if submitted_api_key.is_empty()
        || submitted_api_key == crate::handlers::provider_keys_handler::mask_key(&stored_api_key)
    {
        stored_api_key.clone()
    } else {
        submitted_api_key
    };
    let api_key_changed = api_key != stored_api_key;

    // The account signing key. Three explicit arms, so SET, KEEP and CLEAR are different requests:
    // a paste wins; `clear_public_key` empties it; otherwise a deployment that has no stored key but
    // does have an API key tries to read the account key from Telnyx (best effort, kanban
    // t_38b352f8) and REPORTS the outcome rather than pretending it worked.
    let mut public_key = stored_public_key.clone();
    let mut signing_key_derived = false;
    let mut signing_key_error: Option<String> = None;
    if req.clear_public_key.unwrap_or(false) {
        public_key.clear();
    } else if let Some(supplied) = req
        .public_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        public_key = validate_public_key(supplied).map_err(AppError::BadRequest)?;
    } else if public_key.trim().is_empty() && !api_key.trim().is_empty() {
        match derive_public_key(
            &crate::handlers::message_handler::telnyx_api_base(),
            &api_key,
        )
        .await
        {
            Ok(derived) => {
                public_key = derived;
                signing_key_derived = true;
            }
            Err(reason) => signing_key_error = Some(reason),
        }
    }

    match existing.as_ref().map(|c| c.id) {
        Some(id) => {
            sqlx::query(
                "UPDATE telnyx_config SET api_key = $1, profile_id = COALESCE($2, profile_id), messaging_profile_id = COALESCE($3, messaging_profile_id), public_key = $4, updated_at = NOW() WHERE id = $5"
            )
            .bind(&api_key)
            .bind(&req.profile_id)
            .bind(&req.messaging_profile_id)
            .bind(&public_key)
            .bind(id)
            .execute(&state.pool)
            .await?;
        }
        None => {
            sqlx::query(
                "INSERT INTO telnyx_config (id, api_key, profile_id, messaging_profile_id, public_key, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, NOW(), NOW())"
            )
            .bind(Uuid::new_v4())
            .bind(&api_key)
            .bind(&req.profile_id)
            .bind(&req.messaging_profile_id)
            .bind(&public_key)
            .execute(&state.pool)
            .await?;
        }
    }

    // Answer from the row that is now stored, not from the request: the panel then paints what the
    // delivery path will actually use.
    let saved = get_telnyx_config(&state.pool).await?;
    let (profile_id, messaging_profile_id, saved_api_key) = match saved.as_ref() {
        Some(c) => (
            c.profile_id.clone(),
            c.messaging_profile_id.clone(),
            c.api_key.clone(),
        ),
        None => (
            req.profile_id.clone(),
            req.messaging_profile_id.clone(),
            api_key.clone(),
        ),
    };
    let (resolved, source) =
        resolve_signing_key(state.config.telnyx_public_key.as_deref(), &state.pool).await?;

    Ok(Json(json!({
        "message": "Telnyx configuration updated",
        "api_key": crate::handlers::provider_keys_handler::mask_key(&saved_api_key),
        "api_key_changed": api_key_changed,
        "profile_id": profile_id,
        "messaging_profile_id": messaging_profile_id,
        "has_public_key": resolved.is_some(),
        "public_key_source": source.as_str(),
        "signing_key_derived": signing_key_derived,
        "signing_key_error": signing_key_error,
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

    // -- The account signing key (kanban t_38b352f8) ------------------------------------------------

    /// The exact shape Telnyx issues: base64 of a 32-byte Ed25519 public key. Accepted with the
    /// surrounding whitespace a copy-paste from Mission Control carries, and stored trimmed.
    #[test]
    fn a_32_byte_base64_key_is_accepted_and_trimmed() {
        let key = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        assert_eq!(validate_public_key(&key).as_deref(), Ok(key.as_str()));
        assert_eq!(
            validate_public_key(&format!("  {key}\n")).as_deref(),
            Ok(key.as_str())
        );
        // Padding-free base64 is the same key, and Telnyx's portal has shown both.
        let unpadded = key.trim_end_matches('=').to_string();
        assert_eq!(
            validate_public_key(&unpadded).as_deref(),
            Ok(unpadded.as_str())
        );
    }

    /// A value that cannot be a 32-byte key is refused AT THE WRITE, naming the field, the decoded
    /// length and where the real value comes from — never stored to fail 503 on every delivery.
    #[test]
    fn a_key_of_the_wrong_length_names_the_field_the_length_and_the_source() {
        let short = base64::engine::general_purpose::STANDARD.encode([9u8; 31]);
        let msg = validate_public_key(&short).expect_err("31 bytes must be refused");
        assert!(msg.contains("public_key"), "{msg}");
        assert!(msg.contains("31 bytes") && msg.contains("32"), "{msg}");
        assert!(msg.contains("Telnyx Mission Control"), "{msg}");

        let long = base64::engine::general_purpose::STANDARD.encode([9u8; 33]);
        let msg = validate_public_key(&long).expect_err("33 bytes must be refused");
        assert!(msg.contains("33 bytes"), "{msg}");
    }

    /// An empty value is its own sentence: the caller used the CLEAR arm or sent nothing, and
    /// neither may be confused with a malformed paste.
    #[test]
    fn an_empty_key_and_a_non_base64_key_are_refused_with_distinct_sentences() {
        assert_eq!(
            validate_public_key("   "),
            Err("public_key is empty".to_string())
        );
        let msg = validate_public_key("not base64 at all !!!").expect_err("must be refused");
        assert!(msg.contains("not valid base64"), "{msg}");
        assert!(msg.contains("Telnyx Mission Control"), "{msg}");
    }

    /// A one-shot HTTP stub: answers the next request with `status` + `body`, then closes. Binds an
    /// ephemeral loopback port, so nothing on this host is involved and nothing leaves the box.
    fn stub_http(status: &'static str, body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut socket, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf);
                let answer = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(answer.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    /// The convenience arm reads the account key out of Telnyx's answer — all three shapes the
    /// endpoint has been documented to use — and stores only a VALIDATED 32-byte key.
    #[tokio::test]
    async fn the_derivation_arm_reads_the_key_out_of_every_answer_shape() {
        let key = base64::engine::general_purpose::STANDARD.encode([3u8; 32]);
        for shape in [
            format!("{{\"data\":{{\"public_key\":\"{key}\"}}}}"),
            format!("{{\"data\":[{{\"public_key\":\"{key}\"}}]}}"),
            format!("{{\"public_key\":\"{key}\"}}"),
        ] {
            let body: &'static str = Box::leak(shape.into_boxed_str());
            let base = stub_http("200 OK", body);
            assert_eq!(derive_public_key(&base, "KEY").await, Ok(key.clone()));
        }
    }

    /// A refusal, an unreadable answer or a key that fails validation is REPORTED, never stored:
    /// the operator is told to paste the key instead of believing inbound is configured.
    #[tokio::test]
    async fn the_derivation_arm_reports_an_answer_it_cannot_use() {
        let base = stub_http("401 Unauthorized", "{\"errors\":[{\"detail\":\"nope\"}]}");
        let err = derive_public_key(&base, "KEY")
            .await
            .expect_err("a 401 must be reported");
        assert!(err.contains("401"), "{err}");
        assert!(err.contains("Mission Control"), "{err}");

        // A shape this app does not recognise is reported rather than guessed at.
        let base = stub_http("200 OK", "{\"data\":{\"something_else\":true}}");
        let err = derive_public_key(&base, "KEY")
            .await
            .expect_err("an unknown shape must be reported");
        assert!(err.contains("200"), "{err}");
    }
}
