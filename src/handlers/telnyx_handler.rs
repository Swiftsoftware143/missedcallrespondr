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
pub struct TelnyxWebhookPayload {
    pub data: Option<TelnyxWebhookData>,
    pub meta: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct TelnyxWebhookData {
    pub event_type: Option<String>,
    pub id: Option<String>,
    pub occurred_at: Option<String>,
    pub payload: Option<TelnyxWebhookEventPayload>,
}

#[derive(Debug, Deserialize)]
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
// Helpers
// ---------------------------------------------------------------------------

/// Look up tenant_id by called_number (the number Telnyx dialed in to).
async fn tenant_id_for_number(pool: &sqlx::PgPool, called_number: &str) -> Result<Uuid, AppError> {
    let tenant_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT tenant_id FROM phone_numbers WHERE number = $1 AND is_active = true LIMIT 1"
    )
    .bind(called_number)
    .fetch_optional(pool)
    .await?
    .flatten();

    tenant_id.ok_or_else(|| AppError::NotFound(format!("No tenant found for number: {}", called_number)))
}

/// Deduct one credit from the tenant; returns `false` if balance <= 0 after deduction.
async fn deduct_credit(pool: &sqlx::PgPool, tenant_id: Uuid) -> Result<bool, AppError> {
    let result = sqlx::query_scalar::<_, Option<i32>>(
        "UPDATE tenant_plans
         SET credit_balance = GREATEST(credit_balance - 1, 0),
             lifetime_credits = lifetime_credits + 1,
             updated_at = NOW()
         WHERE tenant_id = $1
         RETURNING credit_balance"
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
async fn get_telnyx_config(pool: &sqlx::PgPool) -> Result<Option<TelnyxConfig>, AppError> {
    let config = sqlx::query_as::<_, TelnyxConfig>(
        "SELECT * FROM telnyx_config LIMIT 1"
    )
    .fetch_optional(pool)
    .await?;
    Ok(config)
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
        event_type, from_number, to_number, call_control_id
    );

    // -- 2. Only process inbound calls
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

    let tenant_id = tenant_id_for_number(&state.pool, &normalized_called).await
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
                tenant_id, from_number.as_deref().unwrap_or("unknown")
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

    // -- 8. Return Telnyx call-control commands (answer + gather)
    tracing::info!(
        "Processed Telnyx call for tenant {}: call_id={}", tenant_id, call_id
    );
    Ok(answer_and_gather_response())
}

// ---------------------------------------------------------------------------
// 2. GET /api/v1/telnyx/numbers — List phone numbers for current tenant
// ---------------------------------------------------------------------------
pub async fn list_numbers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Vec<PhoneNumber>>> {
    let numbers = sqlx::query_as::<_, PhoneNumber>(
        "SELECT id, tenant_id, number, friendly_name, provider, is_active, telnyx_connection_id, created_at, updated_at
         FROM phone_numbers
         WHERE tenant_id = $1 AND (provider = 'telnyx' OR telnyx_connection_id IS NOT NULL)
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

    // Check if already assigned
    let existing = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT id FROM phone_numbers WHERE number = $1 AND is_active = true"
    )
    .bind(&number)
    .fetch_optional(&state.pool)
    .await?;

    if let Some(existing_id) = existing {
        // Already exists — reassign to this tenant if different
        let current_tenant: Option<Uuid> = sqlx::query_scalar(
            "SELECT tenant_id FROM phone_numbers WHERE id = $1"
        )
        .bind(existing_id)
        .fetch_one(&state.pool)
        .await?;

        if current_tenant == Some(tenant_id) {
            return Err(AppError::Conflict("Number already assigned to your account".into()));
        }

        // Reassign
        sqlx::query(
            "UPDATE phone_numbers SET tenant_id = $1, updated_at = NOW() WHERE id = $2"
        )
        .bind(tenant_id)
        .bind(existing_id)
        .execute(&state.pool)
        .await?;

        return Ok(Json(json!({
            "id": existing_id,
            "number": number,
            "assigned": true,
            "reassigned": true
        })));
    }

    // Check if tenant has their own Telnyx key (BYOK) — if so, they manage
    // purchasing on their own Telnyx dashboard; we just register locally.
    let byok = tenant_has_own_telnyx(&state.pool, tenant_id).await?;

    if !byok {
        // Use our Telnyx config to purchase the number via API
        let telnyx_conf = get_telnyx_config(&state.pool).await?
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
        let resp_body: Value = resp.json().await
            .map_err(|e| AppError::Internal(format!("Failed to parse Telnyx response: {}", e)))?;

        if !resp_status.is_success() {
            return Err(AppError::Internal(format!(
                "Telnyx purchase failed ({}): {}",
                resp_status,
                resp_body
            )));
        }
    }

    // Insert the number locally
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

    Ok(Json(json!({
        "id": id,
        "number": number,
        "assigned": true,
        "reassigned": false
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
                .delete(format!("https://api.telnyx.com/v2/phone_numbers/{}", &number.number.trim_start_matches('+')))
                .header("Authorization", format!("Bearer {}", conf.api_key))
                .send()
                .await;
        }
    }

    // Soft-delete (set inactive)
    sqlx::query(
        "UPDATE phone_numbers SET is_active = false, updated_at = NOW() WHERE id = $1"
    )
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
pub async fn get_admin_config(
    State(state): State<AppState>,
) -> ApiResult<Json<Value>> {
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

    // Allow saving empty config — user will set keys later from Super Admin panel
    // if api_key.is_empty() {
    //     return Err(AppError::BadRequest("api_key is required".into()));
    // }

    let existing = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM telnyx_config LIMIT 1"
    )
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
