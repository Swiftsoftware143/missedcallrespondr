//! Admin surface for this app's SYSTEM mail (kanban t_6d575da6).
//!
//!   GET  /api/v1/admin/email-config        — current config, secrets masked; source named
//!   PUT  /api/v1/admin/email-config        — save (a masked secret never clobbers the stored one)
//!   POST /api/v1/admin/email-config/test   — send a real message, return the provider's true answer
//!
//! Mirrors the fleet's panel slot (FunnelSwift's `/api/v1/admin/email-config`, IncentiveSwift's
//! `/api/v1/admin/email-settings`) so the same trade is manageable from the admin panel instead of
//! by editing `/etc/swift/env/missedcall.env`. The `/api/v1/admin/*` prefix is already
//! platform-admin only at the middleware, so these routes inherit that gate.
//!
//! Every response names WHERE the live config came from (`source`: `db` | `env` | `none`): the
//! env file is a stopgap and a panel that cannot tell the two apart would hide it.

use axum::{
    extract::{Extension, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::Claims;
use crate::email_provider;
use crate::error::AppError;
use crate::state::AppState;

/// What the panel shows in place of a stored secret.
const MASK: &str = "••••••••";

fn is_masked(v: &str) -> bool {
    let t = v.trim();
    !t.is_empty() && t.chars().all(|c| c == '•' || c == '*')
}

/// The `admin_settings.email` row as stored, or `{}` when there is none.
async fn stored_row(state: &AppState) -> Result<Value, AppError> {
    let value: Option<Value> =
        sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = $1")
            .bind(email_provider::ADMIN_KEY)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| {
                AppError::Internal(format!(
                    "could not read admin_settings.{}: {e}",
                    email_provider::ADMIN_KEY
                ))
            })?;
    Ok(value.filter(Value::is_object).unwrap_or_else(|| json!({})))
}

/// The env fallback, reported WITHOUT the credential: which host, which From, and whether a key is
/// present at all. An operator sees what is live without a secret ever leaving the process.
fn env_report() -> Value {
    match email_provider::from_env() {
        None => json!({"present": false}),
        Some(cfg) => json!({
            "present": true,
            "provider": cfg.provider,
            "api_url": cfg.api_url,
            "from_address": cfg.from_address,
            "from_name": cfg.from_name,
            "api_key_set": !cfg.api_key.is_empty(),
        }),
    }
}

/// GET /api/v1/admin/email-config — global (system mail) provider config, secrets masked.
pub async fn get_email_config(State(state): State<AppState>) -> Json<Value> {
    let stored = stored_row(&state).await.unwrap_or_else(|_| json!({}));
    let live = email_provider::resolve(&state.pool).await;
    let last_send: Option<Value> =
        sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = $1")
            .bind(email_provider::LAST_SEND_KEY)
            .fetch_optional(&state.pool)
            .await
            .unwrap_or(None);

    let mut config = stored.clone();
    if let Some(obj) = config.as_object_mut() {
        for secret in email_provider::CONFIG_SECRET_FIELDS {
            if obj.contains_key(secret) {
                let set = obj
                    .get(secret)
                    .and_then(|v| v.as_str())
                    .map(|s| !s.is_empty())
                    .unwrap_or(false);
                obj.insert(secret.into(), json!(if set { MASK } else { "" }));
                obj.insert(format!("{secret}_set"), json!(set));
            }
        }
    }

    Json(json!({
        "config": config,
        "stored": stored.as_object().map(|o| !o.is_empty()).unwrap_or(false),
        "configured": live.is_some(),
        "provider": live.as_ref().map(|c| c.provider.clone()).unwrap_or_default(),
        "source": live.as_ref().map(|c| c.source).unwrap_or("none"),
        "env_fallback": env_report(),
        "last_send": last_send.unwrap_or_else(|| json!({"present": false})),
        "providers": email_provider::available(),
    }))
}

/// PUT /api/v1/admin/email-config — save the global system-mail provider.
///
/// A masked secret coming back from the UI never overwrites the stored one; the credential is
/// SEALED before it reaches the database (kanban t_a794cb09 — this row is where the fleet's Mailgun
/// key lives, and a dump must not hand out a working key). The provider must be one this app's
/// transport can actually deliver through, so a save can never select a dead arm.
pub async fn update_email_config(
    State(state): State<AppState>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let existing = stored_row(&state).await?;

    let obj = body
        .as_object_mut()
        .ok_or_else(|| AppError::BadRequest("Expected a JSON object".to_string()))?;

    for secret in email_provider::CONFIG_SECRET_FIELDS {
        let incoming = obj.get(secret).and_then(|v| v.as_str()).unwrap_or("");
        if is_masked(incoming) {
            let kept = existing.get(secret).cloned().unwrap_or_else(|| json!(""));
            obj.insert(secret.to_string(), kept);
        }
        obj.remove(&format!("{secret}_set"));
    }

    // MERGE over the stored row — never a full replace.
    //
    // The panel's save action omits every field the operator left blank, so a replace-style PUT
    // DROPPED `api_url`/`from_address` and left the row incomplete; measured while building this
    // card's proof: saving only a (bad) `api_key` made the row unconfigured, the sender silently
    // fell back to the `EMAIL_*` environment, and the "bad key" control came back 2xx — i.e. the
    // save looked like it worked while the stored credential was never used. Merging keeps the
    // stored transport; an explicit `""` still CLEARS a field, so nothing becomes unreachable.
    let mut merged = existing.as_object().cloned().unwrap_or_default();
    if let Some(incoming) = body.as_object() {
        for (k, v) in incoming {
            merged.insert(k.clone(), v.clone());
        }
    }
    let mut body = Value::Object(merged);

    if let Some(p) = body.get("provider").and_then(|v| v.as_str()) {
        let p = p.trim().to_ascii_lowercase();
        if !p.is_empty()
            && !email_provider::available()
                .iter()
                .any(|v| v.get("value").and_then(|x| x.as_str()) == Some(p.as_str()))
        {
            return Err(AppError::BadRequest(format!(
                "Unknown email provider '{p}'. Choose one of the values served by GET /api/v1/admin/email-config."
            )));
        }
    }

    email_provider::seal_config_secrets(&state.pool, &mut body)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to seal email credentials: {e}")))?;

    sqlx::query(
        "INSERT INTO admin_settings (key, value, description, updated_at)
         VALUES ($1, $2::jsonb, 'Global system email provider (admin-editable)', NOW())
         ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, updated_at = NOW()",
    )
    .bind(email_provider::ADMIN_KEY)
    .bind(body.to_string())
    .execute(&state.pool)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to save email config: {e}")))?;

    let provider = body
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    tracing::info!("admin email-config saved (provider={provider:?})");

    Ok(Json(json!({
        "success": true,
        "provider": provider,
        "configured": email_provider::resolve(&state.pool).await.is_some(),
    })))
}

/// POST /api/v1/admin/email-config/test — send a real message and return the provider's true
/// response (used by the "Send test email" button), so "it saved" and "it can send" are two
/// different answers.
/// Optional explicit recipient for the "Send test email" button: `POST …/email-config/test?to=<addr>`.
///
/// Absent, the test goes to the signed-in admin's own address — behaviour unchanged. Added
/// 2026-10-10 (test-mail-hygiene): an automated fleet probe must be able to aim the test at a
/// disposable mailbox, because otherwise every probe run mails whatever inbox the admin owns.
#[derive(Deserialize)]
pub struct TestRecipientQuery {
    pub to: Option<String>,
}

pub async fn test_email_config(
    State(state): State<AppState>,
    Query(q): Query<TestRecipientQuery>,
    Extension(claims): Extension<Claims>,
) -> Json<Value> {
    let Some(cfg) = email_provider::resolve(&state.pool).await else {
        return Json(json!({
            "success": false,
            "source": "none",
            "detail": "No email provider configured — save provider + credentials first, or set EMAIL_API_URL/EMAIL_API_KEY/EMAIL_FROM."
        }));
    };

    let to = match q.to.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(explicit) => explicit.to_string(),
        None if claims.email.trim().is_empty() => "swiftsoftware143@yahoo.com".to_string(),
        None => claims.email.clone(),
    };

    let subject = "MissedCall Respondr system email test";
    let text = "This is a test of the MissedCall Respondr system email provider.\n\nIf you received it, sending works.\n\n- MissedCall Respondr";
    match email_provider::deliver(&cfg, &to, subject, text, None).await {
        Ok(receipt) => {
            tracing::info!(
                "email.test: provider={} source={} to={to} receipt={receipt:?}",
                cfg.provider,
                cfg.source
            );
            Json(json!({
                "success": true,
                "provider": cfg.provider,
                "source": cfg.source,
                "to": to,
                "status": receipt,
                "detail": format!("{} accepted the message", cfg.provider),
            }))
        }
        Err(e) => {
            tracing::error!(
                "email.test: provider={} source={} to={to} FAILED: {e}",
                cfg.provider,
                cfg.source
            );
            Json(json!({
                "success": false,
                "provider": cfg.provider,
                "source": cfg.source,
                "to": to,
                "detail": e,
            }))
        }
    }
}
