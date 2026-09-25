//! Payment provider management & checkout session creation.
//!
//! Endpoints:
//! - GET    /api/v1/payment-providers          (list configured providers, keys masked)
//! - POST   /api/v1/payment-providers          (create/update — super admin only)
//! - DELETE /api/v1/payment-providers/{provider_type} (remove — super admin only)
//! - POST   /api/v1/checkout/create            (create a Stripe/PayPal checkout session)
//! - GET    /api/v1/checkout/sessions          (list checkout sessions for this tenant)
//! - POST   /api/v1/webhooks/stripe            (Stripe webhook receiver — no auth)
//! - POST /api/v1/webhooks/paypal            (PayPal webhook receiver — no bearer auth, but the
//!   `paypal-transmission-sig` signature IS verified before dispatch; unconfigured -> 503)

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Extension,
};
use base64::{engine::general_purpose, Engine as _};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::config::Claims;
use crate::email;
use crate::error::AppError;
use crate::state::AppState;
use rand::Rng;

type ApiResult<T> = Result<T, AppError>;

// ──────────────────────────────────────────────
// Admin: Payment Provider CRUD
// ──────────────────────────────────────────────

/// GET /api/v1/payment-providers
/// List all configured payment providers (keys masked)
pub async fn list_payment_providers(State(state): State<AppState>) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query(
        r#"SELECT id, provider_type, label, is_active,
                  CASE WHEN api_key_encrypted IS NOT NULL AND api_key_encrypted != '' THEN 'configured' ELSE 'not_configured' END as key_status,
                  COALESCE(publishable_key, '') as publishable_key,
                  is_test_mode, config, created_at, updated_at
           FROM payment_providers
           ORDER BY provider_type ASC"#,
    )
    .fetch_all(&state.pool)
    .await?;

    let providers: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
                "provider_type": r.try_get::<&str,_>("provider_type").unwrap_or(""),
                "label": r.try_get::<&str,_>("label").unwrap_or(""),
                "is_active": r.try_get::<bool,_>("is_active").unwrap_or(false),
                "key_status": r.try_get::<&str,_>("key_status").unwrap_or("not_configured"),
                "publishable_key": r.try_get::<&str,_>("publishable_key").unwrap_or(""),
                "is_test_mode": r.try_get::<bool,_>("is_test_mode").unwrap_or(true),
                "config": r.try_get::<Value,_>("config").unwrap_or(json!({})),
                "created_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("created_at")
                    .map(|t| t.to_rfc3339()).unwrap_or_default(),
                "updated_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("updated_at")
                    .map(|t| t.to_rfc3339()).unwrap_or_default(),
            })
        })
        .collect();

    Ok(Json(json!({"providers": providers})))
}

/// POST /api/v1/payment-providers
/// Create or update a payment provider configuration (super admin only)
pub async fn upsert_payment_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    // Super admin only
    if claims.role != "super_admin" {
        return Err(AppError::Unauthorized(
            "Only super admins can manage payment providers".into(),
        ));
    }

    let provider_type = req
        .get("provider_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            AppError::BadRequest(
                "provider_type is required (stripe, paypal, square, paddle)".into(),
            )
        })?;

    if !["stripe", "paypal", "square", "paddle"].contains(&provider_type) {
        return Err(AppError::BadRequest(
            "Invalid provider_type. Must be stripe, paypal, square, or paddle".into(),
        ));
    }

    let label = req.get("label").and_then(|v| v.as_str()).unwrap_or("");
    let is_active = req
        .get("is_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let is_test_mode = req
        .get("is_test_mode")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let publishable_key = req
        .get("publishable_key")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let config = req.get("config").cloned().unwrap_or(json!({}));
    let api_key = req.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
    let webhook_secret = req
        .get("webhook_secret")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Check if provider already exists
    let existing =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM payment_providers WHERE provider_type = $1")
            .bind(provider_type)
            .fetch_optional(&state.pool)
            .await?;

    if let Some(provider_id) = existing {
        // Update — only overwrite api_key/webhook_secret if provided
        let mut query = String::from(
            "UPDATE payment_providers SET label = $1, is_active = $2, is_test_mode = $3, \
             publishable_key = $4, config = $5, updated_at = NOW()",
        );
        let mut param_idx = 6u8;

        if !api_key.is_empty() {
            query.push_str(&format!(", api_key_encrypted = ${}", param_idx));
            param_idx += 1;
        }
        if !webhook_secret.is_empty() {
            query.push_str(&format!(", webhook_secret_encrypted = ${}", param_idx));
            param_idx += 1;
        }
        query.push_str(&format!(" WHERE id = ${}", param_idx));

        let mut q = sqlx::query(&query)
            .bind(label)
            .bind(is_active)
            .bind(is_test_mode)
            .bind(publishable_key)
            .bind(&config);

        if !api_key.is_empty() {
            q = q.bind(api_key);
        }
        if !webhook_secret.is_empty() {
            q = q.bind(webhook_secret);
        }
        q = q.bind(provider_id);

        q.execute(&state.pool).await?;

        Ok(Json(json!({
            "status": "updated",
            "provider_type": provider_type,
            "message": "Payment provider updated"
        })))
    } else {
        // Insert
        if api_key.is_empty() {
            return Err(AppError::BadRequest(
                "api_key is required when creating a new provider".into(),
            ));
        }

        sqlx::query(
            r#"INSERT INTO payment_providers
               (provider_type, label, is_active, api_key_encrypted, webhook_secret_encrypted,
                publishable_key, config, is_test_mode)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
        )
        .bind(provider_type)
        .bind(label)
        .bind(is_active)
        .bind(api_key)
        .bind(webhook_secret)
        .bind(publishable_key)
        .bind(&config)
        .bind(is_test_mode)
        .execute(&state.pool)
        .await?;

        Ok(Json(json!({
            "status": "created",
            "provider_type": provider_type,
            "message": "Payment provider created"
        })))
    }
}

/// DELETE /api/v1/payment-providers/{provider_type}
/// Remove a payment provider configuration (super admin only)
pub async fn delete_payment_provider(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(provider_type): Path<String>,
) -> ApiResult<impl IntoResponse> {
    // Super admin only
    if claims.role != "super_admin" {
        return Err(AppError::Unauthorized(
            "Only super admins can manage payment providers".into(),
        ));
    }

    let result = sqlx::query("DELETE FROM payment_providers WHERE provider_type = $1")
        .bind(&provider_type)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "Payment provider '{}' not found",
            provider_type
        )));
    }

    Ok(Json(
        json!({"status": "deleted", "provider_type": provider_type}),
    ))
}

// ──────────────────────────────────────────────
// Checkout Session Creation
// ──────────────────────────────────────────────

/// Get active payment provider configuration
async fn get_active_provider(
    pool: &sqlx::PgPool,
    provider_type: &str,
) -> Result<Option<Value>, sqlx::Error> {
    let row = sqlx::query(
        r#"SELECT id, provider_type, api_key_encrypted, publishable_key,
                  webhook_secret_encrypted, config, is_test_mode
           FROM payment_providers
           WHERE provider_type = $1 AND is_active = true
           LIMIT 1"#,
    )
    .bind(provider_type)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "provider_type": r.try_get::<&str,_>("provider_type").unwrap_or(""),
            "api_key": r.try_get::<Option<&str>,_>("api_key_encrypted").unwrap_or(None).unwrap_or(""),
            "publishable_key": r.try_get::<Option<&str>,_>("publishable_key").unwrap_or(None).unwrap_or(""),
            "webhook_secret": r.try_get::<Option<&str>,_>("webhook_secret_encrypted").unwrap_or(None).unwrap_or(""),
            "is_test_mode": r.try_get::<bool,_>("is_test_mode").unwrap_or(true),
        })
    }))
}

/// Read a request header as a `&str`, `""` when it is absent or not valid UTF-8. Header names
/// are matched case-insensitively by `HeaderMap`.
fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}

/// Clip an upstream response body for a single log line. Bodies come from a third party and can be
/// arbitrarily long (or echo our own request), so they never reach the log whole and newlines
/// never break the one-line-per-event format.
fn clip_for_log(raw: &str, max: usize) -> String {
    let flat = raw.replace(['\n', '\r'], " ");
    if flat.chars().count() <= max {
        flat
    } else {
        let head: String = flat.chars().take(max).collect();
        format!("{}…(truncated)", head)
    }
}

/// What PayPal's `verify-webhook-signature` call needs: the REST app credentials for Basic auth,
/// the webhook id the signature must verify against, and a log-only label for where the credentials
/// came from. No value in here is ever logged.
struct PaypalVerifyConfig {
    client_id: String,
    client_secret: String,
    webhook_id: String,
    source: &'static str,
}

/// Resolve the receiver's verification configuration, or `None` when PayPal is genuinely not
/// configured. Order:
///
/// 1. webhook id: `PAYPAL_WEBHOOK_ID` (config), then the `webhook_secret` of an ACTIVE `paypal`
///    row in `payment_providers` — the field the shipped admin console's *Payment providers* panel
///    writes, so enabling PayPal needs no redeploy;
/// 2. credentials: that same row's `api_key_encrypted` as `client_id:client_secret` (exactly the
///    value the checkout leg hands to PayPal as Basic auth), then the optional `PAYPAL_CLIENT_ID` /
///    `PAYPAL_CLIENT_SECRET` env pair.
///
/// A database error propagates — it is never collapsed into "not configured", because that would
/// turn a transient DB failure into a permanent 503 that looks like a config problem.
async fn paypal_verify_config(state: &AppState) -> Result<Option<PaypalVerifyConfig>, AppError> {
    let provider = get_active_provider(&state.pool, "paypal").await?;
    let row_api_key = provider
        .as_ref()
        .map(|p| p["api_key"].as_str().unwrap_or("").to_string())
        .unwrap_or_default();
    let row_webhook_id = provider
        .as_ref()
        .map(|p| {
            p["webhook_secret"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default();

    let (client_id, client_secret, source) = match row_api_key.split_once(':') {
        Some((id, secret)) if !id.is_empty() && !secret.is_empty() => {
            (id.to_string(), secret.to_string(), "payment_providers")
        }
        _ => {
            let env_id = std::env::var("PAYPAL_CLIENT_ID").unwrap_or_default();
            let env_secret = std::env::var("PAYPAL_CLIENT_SECRET").unwrap_or_default();
            if env_id.is_empty() || env_secret.is_empty() {
                return Ok(None);
            }
            (env_id, env_secret, "env")
        }
    };

    let env_webhook_id = state.config.paypal_webhook_id.trim().to_string();
    let webhook_id = if env_webhook_id.is_empty() {
        row_webhook_id
    } else {
        env_webhook_id
    };
    if webhook_id.is_empty() {
        return Ok(None);
    }

    Ok(Some(PaypalVerifyConfig {
        client_id,
        client_secret,
        webhook_id,
        source,
    }))
}

/// POST /api/v1/checkout/create
/// Create a Stripe/PayPal checkout session
pub async fn create_checkout_session(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id: Uuid = claims.aid;
    let user_id: Uuid = claims.sub;

    let purchasable_type = req
        .get("purchasable_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("purchasable_type is required".into()))?;

    // Resolve payment provider: explicit > plan's payment_provider > error
    let provider_type = if let Some(pt) = req.get("provider_type").and_then(|v| v.as_str()) {
        pt.to_string()
    } else if purchasable_type == "plan" {
        if let Some(pid) = req
            .get("purchasable_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT payment_provider FROM plans WHERE id = $1",
            )
            .bind(pid)
            .fetch_optional(&state.pool)
            .await?
            .flatten()
            .ok_or_else(|| {
                AppError::BadRequest(
                    "No provider_type specified and plan has no payment_provider set".into(),
                )
            })?
        } else {
            return Err(AppError::BadRequest(
                "purchasable_id is required for plan checkout".into(),
            ));
        }
    } else {
        return Err(AppError::BadRequest(
            "provider_type is required (stripe, paypal)".into(),
        ));
    };

    let amount = req
        .get("amount")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| AppError::BadRequest("amount is required".into()))?;

    let currency = req
        .get("currency")
        .and_then(|v| v.as_str())
        .unwrap_or("USD");
    let purchasable_id = req
        .get("purchasable_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());

    // `plans` has no thank_you_url column (checked against the live DB): the request's own
    // success_url wins, otherwise every plan falls back to the shared thank-you page.
    let success_url = req
        .get("success_url")
        .and_then(|v| v.as_str())
        .map(|url| url.to_string())
        .unwrap_or_else(|| "/thank-you.html".to_string());

    let cancel_url = req
        .get("cancel_url")
        .and_then(|v| v.as_str())
        .unwrap_or("/");

    let metadata = req.get("metadata").cloned().unwrap_or(json!({}));

    // Get the active provider config
    let provider = get_active_provider(&state.pool, &provider_type)
        .await?
        .ok_or_else(|| {
            AppError::BadRequest(format!("No active {} provider configured", provider_type))
        })?;

    let api_key = provider["api_key"].as_str().unwrap_or("").to_string();
    if api_key.is_empty() {
        return Err(AppError::BadRequest(format!(
            "{} API key not configured",
            provider_type
        )));
    }

    // Create checkout session with the provider
    let provider_session = match provider_type.as_str() {
        "stripe" => {
            create_stripe_session(
                &api_key,
                amount,
                currency,
                purchasable_type,
                &success_url,
                cancel_url,
                &metadata,
            )
            .await?
        }
        "paypal" => {
            create_paypal_session(
                &api_key,
                amount,
                currency,
                purchasable_type,
                &success_url,
                cancel_url,
                &metadata,
            )
            .await?
        }
        _ => {
            return Err(AppError::BadRequest(format!(
                "Checkout not supported for provider type: {}",
                provider_type
            )))
        }
    };

    let provider_session_id = provider_session["id"].as_str().unwrap_or("");
    let checkout_url = provider_session["url"].as_str().unwrap_or("");

    // Store the checkout session in our database
    let session_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO checkout_sessions
           (id, account_id, user_id, provider_type, provider_session_id,
            purchasable_type, purchasable_id, amount, currency, status, metadata)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'pending', $10)"#,
    )
    .bind(session_id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(&provider_type)
    .bind(provider_session_id)
    .bind(purchasable_type)
    .bind(purchasable_id)
    .bind(amount)
    .bind(currency)
    .bind(&metadata)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({
        "session_id": session_id.to_string(),
        "provider_session_id": provider_session_id,
        "checkout_url": checkout_url,
        "provider_type": provider_type,
    })))
}

/// Create a Stripe checkout session via Stripe API
async fn create_stripe_session(
    api_key: &str,
    amount: f64,
    currency: &str,
    purchasable_type: &str,
    success_url: &str,
    cancel_url: &str,
    metadata: &Value,
) -> Result<Value, AppError> {
    let client = reqwest::Client::new();

    // Stripe expects amount in cents
    let amount_cents = (amount * 100.0).round() as u64;

    // Build the line item
    let mut line_item = json!({
        "price_data": {
            "currency": currency.to_lowercase(),
            "product_data": {
                "name": format!("{} purchase", purchasable_type.replace('_', " ")),
            },
            "unit_amount": amount_cents,
        },
        "quantity": 1,
    });

    // Add description from metadata if present
    if let Some(desc) = metadata.get("description").and_then(|v| v.as_str()) {
        line_item["price_data"]["product_data"]["description"] = json!(desc);
    }

    let mut body = json!({
        "mode": "payment",
        "success_url": success_url,
        "cancel_url": cancel_url,
        "line_items": [line_item],
        "metadata": metadata.clone(),
    });

    // Map metadata to Stripe's flat format — all values must be strings
    if let Some(obj) = body["metadata"].as_object_mut() {
        for (_k, v) in obj.iter_mut() {
            if !v.is_string() {
                *v = json!(v.to_string());
            }
        }
    }

    let resp = client
        .post("https://api.stripe.com/v1/checkout/sessions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&to_stripe_form_data(&body))
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Stripe API error: {}", e)))?;

    let status = resp.status();
    let response_body: Value = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to parse Stripe response: {}", e)))?;

    if !status.is_success() {
        let error_msg = response_body["error"]["message"]
            .as_str()
            .unwrap_or("Unknown Stripe error");
        return Err(AppError::Internal(format!("Stripe error: {}", error_msg)));
    }

    Ok(json!({
        "id": response_body["id"].as_str().unwrap_or(""),
        "url": response_body["url"].as_str().unwrap_or(""),
    }))
}

/// Create a PayPal order via PayPal REST API
async fn create_paypal_session(
    api_key: &str,
    amount: f64,
    currency: &str,
    _purchasable_type: &str,
    success_url: &str,
    cancel_url: &str,
    _metadata: &Value,
) -> Result<Value, AppError> {
    let client = reqwest::Client::new();

    // PayPal requires an access token first
    let token_resp = client
        .post("https://api-m.paypal.com/v1/oauth2/token")
        .header(
            "Authorization",
            format!("Basic {}", base64_encode_auth(api_key)),
        )
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("grant_type=client_credentials")
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("PayPal auth error: {}", e)))?;

    let token_body: Value = token_resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to parse PayPal auth response: {}", e)))?;

    let access_token = token_body["access_token"]
        .as_str()
        .ok_or_else(|| AppError::Internal("Failed to get PayPal access token".into()))?;

    // Create the order
    let order_body = json!({
        "intent": "CAPTURE",
        "purchase_units": [{
            "amount": {
                "currency_code": currency.to_uppercase(),
                "value": format!("{:.2}", amount),
            }
        }],
        "payment_source": {
            "paypal": {
                "experience_context": {
                    "payment_method_preference": "IMMEDIATE_PAYMENT_REQUIRED",
                    "landing_page": "LOGIN",
                    "user_action": "PAY_NOW",
                    "return_url": success_url,
                    "cancel_url": cancel_url,
                }
            }
        }
    });

    let order_resp = client
        .post("https://api-m.paypal.com/v2/checkout/orders")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("Content-Type", "application/json")
        .header("PayPal-Request-Id", format!("order-{}", Uuid::new_v4()))
        .json(&order_body)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("PayPal order error: {}", e)))?;

    let order_status = order_resp.status();
    let order_body: Value = order_resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to parse PayPal order response: {}", e)))?;

    if !order_status.is_success() {
        let error_msg = order_body["message"]
            .as_str()
            .or_else(|| order_body["error_description"].as_str())
            .unwrap_or("Unknown PayPal error");
        return Err(AppError::Internal(format!("PayPal error: {}", error_msg)));
    }

    // Get the approval URL from the links
    let approval_url = order_body["links"]
        .as_array()
        .and_then(|links| {
            links
                .iter()
                .find(|l| l["rel"].as_str() == Some("approve"))
                .and_then(|l| l["href"].as_str())
        })
        .unwrap_or("");

    Ok(json!({
        "id": order_body["id"].as_str().unwrap_or(""),
        "url": approval_url,
    }))
}

// ──────────────────────────────────────────────
// Webhook Handlers (public — no auth)
// ──────────────────────────────────────────────

/// Which arm of the Stripe receiver's failure contract fired. `None` means the delivery
/// verified and may be dispatched.
struct StripeRejection {
    /// What the caller is answered with.
    status: StatusCode,
    /// The `reason` in the response body.
    reason: &'static str,
    /// The `payment_webhook_events.status` this arm records. Two distinct values for two
    /// distinct situations: the column could not tell "nothing is configured to verify with"
    /// from "a signature was presented and did not verify" while both were `failed`.
    audit_status: &'static str,
}

/// The Stripe receiver's refusal contract as a pure function of what the deployment holds
/// (`secret`: the active `stripe` row's signing secret, `None` when there is nothing to verify
/// with), what the delivery carried (`signature`: the `Stripe-Signature` header) and the verdict
/// of the HMAC check. Unit-tested below; see `stripe_webhook` for why each status was chosen.
fn stripe_rejection(
    secret: Option<&str>,
    signature: &str,
    signature_ok: bool,
) -> Option<StripeRejection> {
    // Nothing to verify with — the receiver cannot accept ANY event, and that is a state an
    // operator fixes from the admin panel, so asking for a retry is the right call.
    if secret.is_none() {
        return Some(StripeRejection {
            status: StatusCode::SERVICE_UNAVAILABLE,
            reason: "stripe_not_configured",
            audit_status: "not_configured",
        });
    }
    // A genuine Stripe delivery always carries `Stripe-Signature`; without it there is nothing
    // to verify against, and the secret itself is fine, so this gets its own reason.
    if signature.is_empty() {
        return Some(StripeRejection {
            status: StatusCode::SERVICE_UNAVAILABLE,
            reason: "stripe_signature_missing",
            audit_status: "signature_failed",
        });
    }
    if !signature_ok {
        return Some(StripeRejection {
            status: StatusCode::SERVICE_UNAVAILABLE,
            reason: "stripe_signature_verification_failed",
            audit_status: "signature_failed",
        });
    }
    None
}

/// POST /api/v1/webhooks/stripe
/// Handle incoming Stripe webhook events.
///
/// Fail CLOSED and LOUD (kanban t_158bf73d, the same class as t_40b77d6a in WorkflowSwift).
/// Closed: nothing that was not verified reaches `handle_checkout_completed` or
/// `mark_session_expired`. The previous shape accepted a delivery whenever the active `stripe`
/// row carried no signing secret OR the request simply carried no `Stripe-Signature` header, so
/// an anonymous `POST {"type":"checkout.session.expired","data":{"object":{"id":…}}}` moved a
/// pending checkout session's status — measured on the pre-fix binary before this change.
/// Loud: no refusal is answered 2xx. Stripe reads 2xx as "delivered" and never retries it, so
/// the old log-and-200 receiver lost the event for good with only a `payment_webhook_events`
/// row as evidence — a lost payment event, which is not a lost login.
///
/// Per-arm contract, with the reason each status was chosen:
///
/// - no active `stripe` row in `payment_providers`, or one with no signing secret stored ->
///   503 `stripe_not_configured`. The receiver is not able to accept events at all and Stripe
///   retries non-2xx with backoff for up to 3 days, so an event that arrives before the
///   endpoint's secret is pasted in the admin panel is delivered again after it is.
/// - a signing secret IS stored and `Stripe-Signature` is absent -> 503
///   `stripe_signature_missing`.
/// - a signing secret IS stored and the signature does not verify -> 503
///   `stripe_signature_verification_failed`.
///
///   Why 503 here and the 401 that `paypal_webhook` answers for the same-sounding arm: PayPal's
///   verdict is computed by PayPal itself, so `verification_status != "SUCCESS"` is an
///   authoritative third-party statement that the delivery is not a genuine PayPal event and no
///   retry can change that. Stripe's verdict is computed HERE, as an HMAC over a secret this
///   deployment stores, so a mismatch is at least as likely to be OUR misconfiguration — a wrong
///   or stale secret, a rotation the panel has not applied yet — as a forged body, and the two
///   are indistinguishable from the bytes we are handed. Stripe re-signs every retry attempt, so
///   503 is the only answer that lets a repaired secret recover an already-lost receipt; a
///   permanently bad body costs a bounded number of retries (Stripe's own backoff, capped at 3
///   days) and is loud in three places: the ERROR line below, the audit row, and Stripe's
///   dashboard, which flags the endpoint as failing and tells the account owner.
/// - a body that is not JSON -> 400 `Invalid JSON`, kept deliberately AND parsed before
///   verification: a malformed body cannot be a Stripe event whatever the signature says, so 400
///   names the real problem (a proxy or a caller mangling the payload) instead of sending the
///   operator to the signing secret. This arm writes no audit row — there is no event id and no
///   structured body to store — so its record is the ERROR log line plus Stripe's dashboard.
/// - verified -> `200 processed`, and a failure that happens *after* verification (credential
///   delivery) still answers 5xx on purpose, from `handle_checkout_completed`.
///
/// Every refusal except the malformed body records its delivery in `payment_webhook_events` —
/// the only durable record — with a `status` that names WHICH arm fired (`not_configured` vs
/// `signature_failed`) and the reason in `error_message`.
pub async fn stripe_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<impl IntoResponse> {
    let event_body: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(e) => {
            tracing::error!(
                "Stripe webhook rejected — the body is not JSON ({}), so it is not a Stripe \
                 event whatever the signature says. Answering 400; no audit row (no event id, \
                 no structured body to store).",
                e
            );
            return Err(AppError::BadRequest(format!("Invalid JSON: {}", e)));
        }
    };

    let event_type = event_body["type"].as_str().unwrap_or("unknown");
    let event_id = event_body["id"].as_str().unwrap_or("");

    // Read per event, never cached: pasting the endpoint's signing secret makes the very next
    // delivery verify, which is what makes the 503 arm recoverable instead of permanent.
    let provider = get_active_provider(&state.pool, "stripe").await?;
    let signature = header_str(&headers, "stripe-signature");
    let secret = provider
        .as_ref()
        .and_then(|p| p["webhook_secret"].as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let signature_ok = match (secret, signature.is_empty()) {
        (Some(secret), false) => verify_stripe_signature(&body, signature, secret),
        _ => false,
    };
    let rejection = stripe_rejection(secret, signature, signature_ok);

    // Log the delivery either way — a refusal is evidence and belongs in the audit table.
    let audit_status = match &rejection {
        None => "received",
        Some(r) => r.audit_status,
    };
    sqlx::query(
        r#"INSERT INTO payment_webhook_events
           (provider_type, event_type, event_id, raw_body, headers, status, error_message)
           VALUES ('stripe', $1, $2, $3, $4, $5, $6)"#,
    )
    .bind(event_type)
    .bind(event_id)
    .bind(&event_body)
    .bind(json!({"stripe-signature": signature}))
    .bind(audit_status)
    .bind(rejection.as_ref().map(|r| r.reason))
    .execute(&state.pool)
    .await?;

    if let Some(rejection) = rejection {
        if rejection.audit_status == "not_configured" {
            tracing::error!(
                "Stripe webhook receiver is NOT CONFIGURED — no active 'stripe' row in \
                 payment_providers, or that row carries no signing secret, so no event can be \
                 verified. Answering 503 so Stripe retries (up to 3 days); paste the endpoint's \
                 whsec_… in the admin panel's payment gateways and the retries verify. \
                 event_id={}",
                event_id
            );
        } else {
            tracing::error!(
                "Stripe webhook REJECTED — {} (event_id={}). Answering 503 so Stripe retries: if \
                 the stored signing secret is wrong, stale or mid-rotation, the retry is the only \
                 way this receipt is recovered; a forged body just gets retried and refused. \
                 Stripe marks the endpoint failing after repeated non-2xx.",
                rejection.reason,
                event_id
            );
        }
        return Ok((
            rejection.status,
            Json(json!({"status": "rejected", "reason": rejection.reason})),
        ));
    }

    // ── Verified from here on: only now may order state be touched ──
    match event_type {
        "checkout.session.completed" => {
            handle_checkout_completed(&state, &event_body, "stripe").await?;
        }
        "checkout.session.expired" => {
            if let Some(session) = event_body.get("data").and_then(|d| d.get("object")) {
                let provider_session_id = session["id"].as_str().unwrap_or("");
                mark_session_expired(&state.pool, "stripe", provider_session_id).await?;
            }
        }
        _ => {
            sqlx::query("UPDATE payment_webhook_events SET status = 'ignored' WHERE event_id = $1")
                .bind(event_id)
                .execute(&state.pool)
                .await?;
        }
    }

    Ok((StatusCode::OK, Json(json!({"status": "processed"}))))
}

/// POST /api/v1/webhooks/paypal
/// Handle incoming PayPal webhook events.
///
/// Fail closed, in this order, BEFORE anything is written or dispatched:
/// any of the four `paypal-*` transmission headers missing -> 401
/// `missing_paypal_signature_headers`; no webhook id, or no REST credentials to verify with
/// -> 503 `paypal_not_configured` (PayPal is not called; no row is written); PayPal answered
/// our verification call non-2xx -> 401 `paypal_verification_api_error`; the verification call
/// could not complete -> 401 `paypal_verification_unreachable`;
/// `verification_status != "SUCCESS"` -> 401 `signature_verification_failed`.
/// Only a verified event reaches `payment_webhook_events` and `handle_checkout_completed`.
///
/// Why this arm exists at all (kanban t_5cf44e1b): the receiver used to read one header for a
/// log line and then INSERT the caller-supplied JSON and dispatch fulfilment on it, so an
/// anonymous `POST {"event_type":"PAYMENT.CAPTURE.COMPLETED","resource":{"id":…}}` completed a
/// checkout session and reached fulfilment with no signature whatsoever.
pub async fn paypal_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<impl IntoResponse> {
    let event_body: Value = serde_json::from_slice(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid JSON: {}", e)))?;

    // ── PayPal signature verification (fail closed) ──
    let trans_id = header_str(&headers, "paypal-transmission-id");
    let trans_time = header_str(&headers, "paypal-transmission-time");
    let trans_sig = header_str(&headers, "paypal-transmission-sig");
    let cert_url = header_str(&headers, "paypal-cert-url");

    if trans_id.is_empty() || trans_time.is_empty() || trans_sig.is_empty() || cert_url.is_empty() {
        tracing::error!("PayPal webhook rejected — missing signature headers");
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(json!({"status": "rejected", "reason": "missing_paypal_signature_headers"})),
        ));
    }

    // Resolve what the signature must be verified against. With no webhook id or no credentials
    // there is nothing to verify with, so the receiver says so (503) instead of answering 200 as
    // if it had processed the event.
    let verify_cfg = match paypal_verify_config(&state).await? {
        Some(cfg) => cfg,
        None => {
            tracing::error!(
                "PayPal webhook receiver is NOT CONFIGURED — no PAYPAL_WEBHOOK_ID and no webhook \
                 id on an active 'paypal' row in payment_providers, or no \
                 PAYPAL_CLIENT_ID/PAYPAL_CLIENT_SECRET and no usable api_key on that row. \
                 Rejecting without calling PayPal (fail closed)."
            );
            return Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"status": "rejected", "reason": "paypal_not_configured"})),
            ));
        }
    };

    // PayPal verifies against the webhook id it was configured with, so which id is sent is the
    // difference between a real verdict and a blanket 401. PAYPAL_API_BASE is read per request so
    // the leg is sandbox- and test-addressable.
    let paypal_api_base =
        std::env::var("PAYPAL_API_BASE").unwrap_or_else(|_| "https://api-m.paypal.com".to_string());

    let verify_payload = json!({
        "auth_algo": header_str(&headers, "paypal-auth-algo"),
        "cert_url": cert_url,
        "transmission_id": trans_id,
        "transmission_sig": trans_sig,
        "transmission_time": trans_time,
        "webhook_id": verify_cfg.webhook_id,
        "webhook_event": &event_body,
    });

    // Bounded: a webhook task must not hang on a stalled third party. A timeout lands in the
    // transport arm below, which is a rejection, so the failure direction stays closed.
    let verify_resp = reqwest::Client::new()
        .post(format!(
            "{}/v1/notifications/verify-webhook-signature",
            paypal_api_base
        ))
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .basic_auth(&verify_cfg.client_id, Some(&verify_cfg.client_secret))
        .json(&verify_payload)
        .send()
        .await;

    match verify_resp {
        Ok(resp) => {
            let status = resp.status();
            let raw = resp.text().await.unwrap_or_default();
            if !status.is_success() {
                tracing::error!(
                    "PayPal verify-webhook-signature answered non-2xx (status={}, body={}) — \
                     rejecting. Credentials source: {}",
                    status,
                    clip_for_log(&raw, 300),
                    verify_cfg.source
                );
                return Ok((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"status": "rejected", "reason": "paypal_verification_api_error"})),
                ));
            }
            let body: Value = serde_json::from_str(&raw).unwrap_or_default();
            if body["verification_status"] != "SUCCESS" {
                tracing::error!("PayPal webhook signature verification failed: {:?}", body);
                return Ok((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"status": "rejected", "reason": "signature_verification_failed"})),
                ));
            }
        }
        Err(e) => {
            tracing::error!(
                "PayPal verify-webhook-signature call could not complete (transport error: {}) — \
                 rejecting. Credentials source: {}",
                e,
                verify_cfg.source
            );
            return Ok((
                StatusCode::UNAUTHORIZED,
                Json(json!({"status": "rejected", "reason": "paypal_verification_unreachable"})),
            ));
        }
    }

    // ── Verified: log the event and dispatch ──
    let event_type = event_body["event_type"].as_str().unwrap_or("unknown");
    let event_id = event_body["id"].as_str().unwrap_or("");

    // Log the webhook event
    let mut hdrs = json!({});
    if let Some(trans_id) = headers
        .get("paypal-transmission-id")
        .and_then(|v| v.to_str().ok())
    {
        hdrs["paypal-transmission-id"] = json!(trans_id);
    }

    sqlx::query(
        r#"INSERT INTO payment_webhook_events
           (provider_type, event_type, event_id, raw_body, headers, status)
           VALUES ('paypal', $1, $2, $3, $4, 'received')"#,
    )
    .bind(event_type)
    .bind(event_id)
    .bind(&event_body)
    .bind(&hdrs)
    .execute(&state.pool)
    .await?;

    match event_type {
        "CHECKOUT.ORDER.APPROVED" | "PAYMENT.CAPTURE.COMPLETED" => {
            handle_checkout_completed(&state, &event_body, "paypal").await?;
        }
        _ => {
            sqlx::query("UPDATE payment_webhook_events SET status = 'ignored' WHERE event_id = $1")
                .bind(event_id)
                .execute(&state.pool)
                .await?;
        }
    }

    Ok((StatusCode::OK, Json(json!({"status": "processed"}))))
}

// ──────────────────────────────────────────────
// List Checkout Sessions
// ──────────────────────────────────────────────

/// GET /api/v1/checkout/sessions
/// List checkout sessions for the authenticated tenant
pub async fn list_checkout_sessions(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id: Uuid = claims.aid;

    let rows = sqlx::query(
        r#"SELECT id, account_id, user_id, provider_type, purchasable_type,
                  purchasable_id::text, amount::text, currency, status,
                  provider_session_id, webhook_event_id, webhook_received_at,
                  created_at, updated_at
           FROM checkout_sessions
           WHERE account_id = $1
           ORDER BY created_at DESC
           LIMIT 50"#,
    )
    .bind(tenant_id)
    .fetch_all(&state.pool)
    .await?;

    let sessions: Vec<Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "account_id": r.try_get::<Uuid,_>("account_id").map(|u| u.to_string()).unwrap_or_default(),
            "user_id": r.try_get::<Uuid,_>("user_id").map(|u| u.to_string()).unwrap_or_default(),
            "provider_type": r.try_get::<&str,_>("provider_type").unwrap_or(""),
            "purchasable_type": r.try_get::<&str,_>("purchasable_type").unwrap_or(""),
            "purchasable_id": r.try_get::<Option<&str>,_>("purchasable_id").unwrap_or(None),
            "amount": r.try_get::<&str,_>("amount").unwrap_or("0"),
            "currency": r.try_get::<&str,_>("currency").unwrap_or(""),
            "status": r.try_get::<&str,_>("status").unwrap_or(""),
            "provider_session_id": r.try_get::<Option<&str>,_>("provider_session_id").unwrap_or(None),
            "webhook_event_id": r.try_get::<Option<&str>,_>("webhook_event_id").unwrap_or(None),
            "webhook_received_at": r.try_get::<Option<chrono::DateTime<chrono::Utc>>,_>("webhook_received_at")
                .unwrap_or(None)
                .map(|t| t.to_rfc3339()),
            "created_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("created_at")
                .map(|t| t.to_rfc3339()).unwrap_or_default(),
            "updated_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("updated_at")
                .map(|t| t.to_rfc3339()).unwrap_or_default(),
        })
    }).collect();

    Ok(Json(json!({"sessions": sessions})))
}

// ──────────────────────────────────────────────
// Internal helpers
// ──────────────────────────────────────────────

// ──────────────────────────────────────────────
// Credential delivery helpers
// ──────────────────────────────────────────────

/// Generate a random temporary password (12 characters, alphanumeric)
pub fn generate_temp_password() -> String {
    const CHARSET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!@#$%^&*()";
    let mut rng = rand::thread_rng();
    (0..12)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// Hash a password using argon2
pub fn hash_password(password: &str) -> Result<String, AppError> {
    use argon2::{
        password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
        Argon2,
    };
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(format!("Password hashing failed: {}", e)))?;
    Ok(hash.to_string())
}

/// Deliver credentials to the user who just completed a purchase.
/// - If user exists with password_hash → send purchase_confirmed email
/// - If user exists without password_hash → generate temp password, hash, update, send welcome
/// - If no user → create tenant, create user, send welcome
///
/// The welcome send uses `template_type = "welcome_credentials"`, NOT `"welcome"`: this is the one
/// flow where the password is GENERATED for the customer and is unknowable to them, so the email is
/// the only place it can be delivered. Self-serve signup (`auth::register`) sends `"welcome"`, whose
/// default row deliberately carries no password placeholder — the user chose that password two
/// seconds earlier, and emailing a user-chosen secret only spreads it (card t_46d8d40e).
async fn deliver_credentials(
    state: &AppState,
    email: &str,
    customer_name: &str,
    account_id: Uuid,
    purchasable_type: &str,
) -> Result<(), AppError> {
    // Derive a plan name from purchasable_type
    let plan_name = purchasable_type.replace(['_', '-'], " ");
    let plan_name = plan_name
        .split_whitespace()
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    // Look for existing user by email
    let existing_user =
        sqlx::query("SELECT id, password_hash, name, tenant_id FROM users WHERE email = $1")
            .bind(email)
            .fetch_optional(&state.pool)
            .await?;

    if let Some(user_row) = existing_user {
        let user_id: Uuid = user_row.try_get("id")?;
        let existing_hash: String = user_row.try_get("password_hash")?;
        let existing_name: String = user_row.try_get("name")?;
        let tenant_id: Uuid = user_row.try_get("tenant_id")?;

        if existing_hash.is_empty() || existing_hash.is_empty() {
            // User exists but no password set → generate temp password
            let temp_password = generate_temp_password();
            let hash = hash_password(&temp_password)?;
            sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
                .bind(&hash)
                .bind(user_id)
                .execute(&state.pool)
                .await?;

            let vars = json!({
                "name": &existing_name,
                "email": email,
                "password": &temp_password,
                "app_url": "https://app.missedcallrespondr.com",
            });
            if let Err(e) = email::send_template_email(
                &state.pool,
                tenant_id,
                email,
                "welcome_credentials",
                &vars,
            )
            .await
            {
                tracing::warn!("Failed to send welcome email to {}: {}", email, e);
            }
        } else {
            // User exists with password → send purchase confirmed
            let vars = json!({
                "name": &existing_name,
                "plan_name": plan_name,
                "app_url": "https://app.missedcallrespondr.com",
            });
            if let Err(e) = email::send_template_email(
                &state.pool,
                tenant_id,
                email,
                "purchase_confirmed",
                &vars,
            )
            .await
            {
                tracing::warn!(
                    "Failed to send purchase confirmed email to {}: {}",
                    email,
                    e
                );
            }
        }
    } else {
        // No user found → create tenant + user
        let slug = format!(
            "tenant-{}",
            account_id.to_string().split('-').next().unwrap_or("new")
        );
        let tenant_id = Uuid::new_v4();
        sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
            .bind(tenant_id)
            .bind(customer_name)
            .bind(&slug)
            .execute(&state.pool)
            .await?;

        let temp_password = generate_temp_password();
        let hash = hash_password(&temp_password)?;
        let user_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO users (id, email, password_hash, name, tenant_id, role) VALUES ($1, $2, $3, $4, $5, 'admin')"
        )
        .bind(user_id)
        .bind(email)
        .bind(&hash)
        .bind(customer_name)
        .bind(tenant_id)
        .execute(&state.pool)
        .await?;

        let vars = json!({
            "name": customer_name,
            "email": email,
            "password": &temp_password,
            "app_url": "https://app.missedcallrespondr.com",
        });
        if let Err(e) =
            email::send_template_email(&state.pool, tenant_id, email, "welcome_credentials", &vars)
                .await
        {
            tracing::warn!("Failed to send welcome email to {}: {}", email, e);
        }
    }

    Ok(())
}

/// Handle a completed checkout — update session status and trigger credential delivery
async fn handle_checkout_completed(
    state: &AppState,
    event_body: &Value,
    provider_type: &str,
) -> Result<(), AppError> {
    let session = match provider_type {
        "stripe" => event_body["data"]["object"].clone(),
        "paypal" => event_body["resource"].clone(),
        _ => return Ok(()),
    };

    let provider_session_id = match provider_type {
        "stripe" => session["id"].as_str().map(|s| s.to_string()),
        "paypal" => session["id"].as_str().map(|s| s.to_string()),
        _ => None,
    };

    let Some(provider_session_id) = provider_session_id else {
        tracing::warn!("Webhook received without provider session ID");
        return Ok(());
    };

    // Update the checkout session status
    let result = sqlx::query(
        r#"UPDATE checkout_sessions
           SET status = 'completed',
               webhook_received_at = NOW(),
               webhook_event_id = $1,
               updated_at = NOW()
           WHERE provider_session_id = $2
             AND provider_type = $3
             AND status = 'pending'"#,
    )
    .bind(event_body["id"].as_str().unwrap_or(""))
    .bind(&provider_session_id)
    .bind(provider_type)
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        tracing::warn!(
            "No pending checkout session found for provider session: {}",
            provider_session_id
        );
        return Ok(());
    }

    // Mark the webhook event as processed
    sqlx::query("UPDATE payment_webhook_events SET status = 'processed' WHERE event_id = $1")
        .bind(event_body["id"].as_str().unwrap_or(""))
        .execute(&state.pool)
        .await?;

    // ── Credential delivery ──
    // Query the checkout session to get account_id and metadata
    let session_row = sqlx::query(
        r#"SELECT account_id, user_id, metadata FROM checkout_sessions
           WHERE provider_session_id = $1 AND provider_type = $2"#,
    )
    .bind(&provider_session_id)
    .bind(provider_type)
    .fetch_optional(&state.pool)
    .await?;

    if let Some(row) = session_row {
        let account_id: Uuid = row.try_get("account_id")?;
        let metadata: Value = row.try_get("metadata")?;
        let customer_email = metadata.get("customer_email").and_then(|v| v.as_str());
        let purchasable_type: String = {
            let ptype: String = sqlx::query_scalar(
                "SELECT purchasable_type FROM checkout_sessions WHERE provider_session_id = $1 AND provider_type = $2"
            )
            .bind(&provider_session_id)
            .bind(provider_type)
            .fetch_one(&state.pool)
            .await?;
            ptype
        };

        if let Some(email) = customer_email {
            let customer_name = metadata
                .get("customer_name")
                .and_then(|v| v.as_str())
                .unwrap_or(email.split('@').next().unwrap_or("Customer"));

            if let Err(e) =
                deliver_credentials(state, email, customer_name, account_id, &purchasable_type)
                    .await
            {
                tracing::warn!("Credential delivery failed for {}: {:?}", email, e);
            }
        }
    }

    // ── Fire affiliate conversion (if referral metadata present) ──
    let session_meta = sqlx::query_scalar::<_, Value>(
        r#"SELECT COALESCE(metadata, '{}'::jsonb) FROM checkout_sessions
           WHERE provider_session_id = $1 AND provider_type = $2"#,
    )
    .bind(&provider_session_id)
    .bind(provider_type)
    .fetch_optional(&state.pool)
    .await?;

    if let Some(meta) = session_meta {
        let affiliate_id = meta
            .get("affiliate_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let cookie_id = meta
            .get("cookie_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        if affiliate_id.is_some() || cookie_id.is_some() {
            let amount = session["amount_total"]
                .as_f64()
                .map(|v| v / 100.0)
                .or_else(|| {
                    session["amount"]
                        .as_str()
                        .and_then(|s| s.parse::<f64>().ok())
                });

            let payload = serde_json::json!({
                "affiliate_id": affiliate_id,
                "cookie_id": cookie_id,
                "source_app": "missedcallrespondr",
                "event": "checkout_completed",
                "product_id": meta.get("product_id").and_then(|v| v.as_str()),
                "product_name": meta.get("product_name").and_then(|v| v.as_str()),
                "amount": amount,
                "lead_email": meta.get("customer_email").and_then(|v| v.as_str()),
            });

            match reqwest::Client::new()
                .post(format!(
                    "{}/api/v1/webhooks/conversion",
                    state.funnelswift_url
                ))
                .header("X-Internal-Key", &state.config.internal_sync_key)
                .header("Content-Type", "application/json")
                .json(&payload)
                .send()
                .await
            {
                Ok(resp) => {
                    tracing::info!("Affiliate conversion fired — status: {}", resp.status())
                }
                Err(e) => tracing::warn!("Failed to fire affiliate conversion: {:?}", e),
            }
        }
    }

    tracing::info!(
        "Checkout completed: provider_session={}",
        provider_session_id
    );
    Ok(())
}

/// Mark a checkout session as expired
async fn mark_session_expired(
    pool: &sqlx::PgPool,
    provider_type: &str,
    provider_session_id: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE checkout_sessions SET status = 'expired', updated_at = NOW() \
         WHERE provider_session_id = $1 AND provider_type = $2 AND status = 'pending'",
    )
    .bind(provider_session_id)
    .bind(provider_type)
    .execute(pool)
    .await?;

    Ok(())
}

/// GET /api/v1/checkout/session/:id
///
/// Public, id-scoped lookup used by `thank-you.html` after the payment provider
/// redirects the buyer back. Exposes only presentation-safe fields — never
/// account_id, user_id, provider_session_id or metadata.
pub async fn get_checkout_session_public(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let id = Uuid::parse_str(&session_id)
        .map_err(|_| AppError::NotFound("Checkout session not found".into()))?;

    let row = sqlx::query(
        r#"SELECT cs.id, cs.purchasable_type, cs.amount::text AS amount,
                  cs.currency, cs.status, cs.created_at,
                  p.name AS plan_name
           FROM checkout_sessions cs
           LEFT JOIN plans p
                  ON cs.purchasable_type = 'plan' AND p.id = cs.purchasable_id
           WHERE cs.id = $1"#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;

    let Some(row) = row else {
        tracing::warn!("checkout session lookup miss: {}", id);
        return Ok((
            StatusCode::NOT_FOUND,
            Json(json!({
                "found": false,
                "error": "checkout_session_not_found",
                "note": "No checkout session with that id. If you just paid, your provider receipt is authoritative — contact support if your plan is not active."
            })),
        )
            .into_response());
    };

    Ok((
        StatusCode::OK,
        Json(json!({
            "found": true,
            "id": row.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "status": row.try_get::<&str,_>("status").unwrap_or(""),
            "plan_name": row.try_get::<Option<String>,_>("plan_name").unwrap_or(None),
            "purchasable_type": row.try_get::<&str,_>("purchasable_type").unwrap_or(""),
            "amount": row.try_get::<&str,_>("amount").unwrap_or("0"),
            "currency": row.try_get::<&str,_>("currency").unwrap_or("USD"),
            "login_url": "/login.html",
            "created_at": row
                .try_get::<chrono::DateTime<chrono::Utc>,_>("created_at")
                .map(|t| t.to_rfc3339())
                .unwrap_or_default(),
        })),
    )
        .into_response())
}

fn verify_stripe_signature(body: &[u8], signature: &str, secret: &str) -> bool {
    use ring::hmac;

    // Stripe sends signatures in the format: t=timestamp,v1=signature
    let parts: Vec<&str> = signature.split(',').collect();
    let mut timestamp = "";
    let mut expected_sig = "";

    for part in &parts {
        if let Some(t) = part.strip_prefix("t=") {
            timestamp = t;
        } else if let Some(s) = part.strip_prefix("v1=") {
            expected_sig = s;
        }
    }

    if timestamp.is_empty() || expected_sig.is_empty() {
        return false;
    }

    // Build the payload: timestamp + "." + body
    let body_str = std::str::from_utf8(body).unwrap_or("");
    let payload = format!("{}.{}", timestamp, body_str);

    // Compute HMAC-SHA256 using ring
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let computed = hmac::sign(&key, payload.as_bytes());
    let computed_hex = hex::encode(computed.as_ref());

    computed_hex == expected_sig
}

/// Convert a JSON value to URL-encoded form data for Stripe API
fn to_stripe_form_data(value: &Value) -> Vec<(String, String)> {
    let mut pairs = Vec::new();

    fn flatten(prefix: &str, value: &Value, pairs: &mut Vec<(String, String)>) {
        match value {
            Value::Object(map) => {
                for (k, v) in map {
                    let key = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{}[{}]", prefix, k)
                    };
                    flatten(&key, v, pairs);
                }
            }
            Value::Array(arr) => {
                for (i, v) in arr.iter().enumerate() {
                    let key = format!("{}[{}]", prefix, i);
                    flatten(&key, v, pairs);
                }
            }
            Value::String(s) => {
                pairs.push((prefix.to_string(), s.clone()));
            }
            Value::Number(n) => {
                pairs.push((prefix.to_string(), n.to_string()));
            }
            Value::Bool(b) => {
                pairs.push((prefix.to_string(), b.to_string()));
            }
            Value::Null => {
                pairs.push((prefix.to_string(), String::new()));
            }
        }
    }

    flatten("", value, &mut pairs);
    pairs
}

/// Base64-encode a client_id:secret pair for PayPal Basic auth
fn base64_encode_auth(credentials: &str) -> String {
    general_purpose::STANDARD.encode(credentials.as_bytes())
}
#[cfg(test)]
mod stripe_contract_tests {
    //! The per-arm refusal contract of `stripe_webhook` (kanban t_158bf73d), pinned as a pure
    //! function so the status, the response reason and the audit `status` can never drift apart:
    //! the whole point of the card is that a refusal must not be a 2xx (Stripe would never
    //! retry) and that the audit row must name WHICH arm fired.
    use super::*;

    fn arm(secret: Option<&str>, sig: &str, ok: bool) -> (&'static str, u16, &'static str) {
        let r = stripe_rejection(secret, sig, ok).expect("expected a refusal");
        (r.reason, r.status.as_u16(), r.audit_status)
    }

    #[test]
    fn nothing_to_verify_with_is_not_configured_and_retryable() {
        assert_eq!(
            arm(None, "", false),
            ("stripe_not_configured", 503, "not_configured")
        );
        // A signature that would have verified does not matter: with no secret there is nothing
        // to verify it against, so the arm is still not_configured.
        assert_eq!(
            arm(None, "t=1,v1=deadbeef", true),
            ("stripe_not_configured", 503, "not_configured")
        );
    }

    #[test]
    fn a_stored_secret_with_no_signature_header_is_its_own_arm() {
        assert_eq!(
            arm(Some("whsec_x"), "", false),
            ("stripe_signature_missing", 503, "signature_failed")
        );
    }

    #[test]
    fn a_bad_signature_is_503_not_401() {
        // 503 (not the 401 paypal_webhook answers) so Stripe retries: the verdict is our own
        // HMAC over a secret we store, so a mismatch may well be our misconfiguration.
        assert_eq!(
            arm(Some("whsec_x"), "t=1,v1=0000", false),
            (
                "stripe_signature_verification_failed",
                503,
                "signature_failed"
            )
        );
    }

    #[test]
    fn a_verified_delivery_is_admitted() {
        assert!(stripe_rejection(Some("whsec_x"), "t=1,v1=0000", true).is_none());
    }

    #[test]
    fn the_hmac_check_still_accepts_a_genuine_signature_and_rejects_a_forged_one() {
        use ring::hmac;
        let body = br#"{"id":"evt_1","type":"customer.created"}"#;
        let secret = "whsec_unit_test";
        let ts = "1700000000";
        let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
        let mac = hmac::sign(
            &key,
            format!("{}.{}", ts, String::from_utf8_lossy(body)).as_bytes(),
        );
        let good = format!("t={},v1={}", ts, hex::encode(mac.as_ref()));

        assert!(verify_stripe_signature(body, &good, secret));
        assert!(!verify_stripe_signature(body, &good, "whsec_other"));
        assert!(!verify_stripe_signature(
            body,
            "t=1700000000,v1=0000",
            secret
        ));
        assert!(!verify_stripe_signature(body, "v1=0000", secret));
    }
}
