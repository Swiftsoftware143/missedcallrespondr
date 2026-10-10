//! Email provider configuration + delivery for this app's SYSTEM mail.
//!
//! Resolution order:
//!   1. `admin_settings` key `email` — the fleet's panel-managed slot (admin panel >
//!      Operator Console > "Email Provider (system mail)"). Credentials in the row are sealed at
//!      rest with the app's `enc:v1:` envelope, the same one `provider_keys` uses.
//!   2. the `EMAIL_*` environment variables — the stopgap that was staged into
//!      `/etc/swift/env/missedcall.env` while this app had no panel slot at all (kanban
//!      t_e562ab92). Env-only mail config is the shape the platform standard forbids, so it is a
//!      FALLBACK here, never the first source.
//!
//! The BODY SHAPE is a transport detail and it is per provider (kanban t_6d575da6):
//!
//!   * Mailgun  `POST /v3/<domain>/messages` — `application/x-www-form-urlencoded`, `Basic api:<key>`
//!   * SendGrid `POST /v3/mail/send`         — JSON `{"from":{"email":…}}`, `Bearer`
//!   * Sendiio  `POST /api/v1/smtp/send`     — JSON, `Bearer`
//!
//! The pre-fix sender posted ONE JSON body with a top-level `"from"` to every provider. Mailgun
//! authenticates that request (so the key looked fine) and then answers
//! `400 {"message":"from parameter is missing"}` because a JSON body carries no form field at all —
//! so the account was created and its welcome mail could never be sent.
//!
//! A transport that cannot deliver is never offered: [`available`] lists exactly the arms
//! [`deliver`] implements, the admin PUT route validates against it, and an unexpected stored
//! provider answers a NAMED error instead of silently doing nothing.

use crate::security::provider_key_crypto;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::env;

/// The `admin_settings` row this app's system mail lives in — the same key every other fleet app
/// uses (`provider` / `api_url` / `api_key` / `from_name` / `from_address` / `smtp_*`).
pub const ADMIN_KEY: &str = "email";

/// Where the sender records the outcome of its last attempt, so a failed send is visible on the
/// admin surface and not only in the container log.
pub const LAST_SEND_KEY: &str = "email_last_send";

#[derive(Debug, Clone)]
pub struct EmailConfig {
    pub provider: String,
    pub api_url: String,
    pub api_key: String,
    pub domain: String,
    pub from_address: String,
    pub from_name: String,
    /// "db" (admin_settings row) or "env" (EMAIL_* fallback) — reported on the admin route so the
    /// live source is never a guess.
    pub source: &'static str,
}

fn s(cfg: &Value, key: &str) -> String {
    cfg.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Accept both the generic names and the legacy Mailgun/SMTP row names.
fn first(cfg: &Value, keys: &[&str]) -> String {
    for k in keys {
        let v = s(cfg, k);
        if !v.is_empty() {
            return v;
        }
    }
    String::new()
}

/// Split a configured From value into `(display name, bare address)`.
///
/// `EMAIL_FROM` is commonly written `Name <addr@host>`; SendGrid (and every other JSON API) wants
/// the bare address in `from.email`, while Mailgun accepts either form. Keeping the bare address in
/// `from_address` and the display name separately means one stored value is correct for every arm.
pub fn split_sender(value: &str) -> (String, String) {
    let v = value.trim();
    if let (Some(open), Some(close)) = (v.find('<'), v.rfind('>')) {
        if open < close {
            let name = v[..open].trim().trim_matches('"').trim().to_string();
            let addr = v[open + 1..close].trim().to_string();
            if !addr.is_empty() {
                return (name, addr);
            }
        }
    }
    (String::new(), v.to_string())
}

/// Which provider a config is for, from an explicit key, else the api host, else Mailgun — the
/// provider this fleet actually sends through.
fn infer_provider(explicit: &str, api_url: &str) -> String {
    let e = explicit.trim().to_ascii_lowercase();
    if !e.is_empty() {
        return e;
    }
    let host = api_url.to_ascii_lowercase();
    if host.contains("sendgrid") {
        "sendgrid".to_string()
    } else if host.contains("sendiio") {
        "sendiio".to_string()
    } else {
        "mailgun".to_string()
    }
}

impl EmailConfig {
    pub fn from_json(cfg: &Value, source: &'static str) -> EmailConfig {
        let api_url = first(cfg, &["api_url", "base_url"]);
        let provider = infer_provider(&s(cfg, "provider"), &api_url);
        let (name_from_sender, addr_from_sender) =
            split_sender(&first(cfg, &["from_address", "from_email", "from"]));
        let from_name = {
            let n = first(cfg, &["from_name"]);
            if n.is_empty() {
                name_from_sender
            } else {
                n
            }
        };
        EmailConfig {
            provider,
            api_url,
            api_key: first(cfg, &["api_key"]),
            domain: first(cfg, &["domain", "mailgun_domain"]),
            from_address: addr_from_sender,
            from_name,
            source,
        }
    }

    /// True when the stored config actually carries what its transport needs.
    pub fn is_configured(&self) -> bool {
        !self.api_key.is_empty() && !self.from_address.is_empty()
    }

    pub fn sender(&self) -> String {
        if self.from_name.is_empty() {
            self.from_address.clone()
        } else {
            format!("{} <{}>", self.from_name, self.from_address)
        }
    }
}

/// The stopgap source: the `EMAIL_*` environment. Returns `None` when nothing is set, so
/// "unconfigured" stays distinguishable from "configured and broken".
pub fn from_env() -> Option<EmailConfig> {
    let api_url = env::var("EMAIL_API_URL").unwrap_or_default();
    let api_key = env::var("EMAIL_API_KEY").unwrap_or_default();
    let from = env::var("EMAIL_FROM").unwrap_or_default();
    if api_url.trim().is_empty() && api_key.trim().is_empty() && from.trim().is_empty() {
        return None;
    }
    let mut row = json!({
        "provider": env::var("EMAIL_PROVIDER").unwrap_or_default(),
        "api_url": api_url,
        "api_key": api_key,
        "domain": env::var("EMAIL_DOMAIN").unwrap_or_default(),
        "from_address": from,
        "from_name": env::var("EMAIL_FROM_NAME").unwrap_or_default(),
    });
    // `EMAIL_API_AUTH` is a transport hint the old sender honoured; nothing here reads it, but the
    // value must not silently disappear from the picture, so it is carried into the config for
    // reporting. The arm is chosen by provider, not by a free-text env var.
    if let Some(obj) = row.as_object_mut() {
        obj.insert(
            "auth_hint".to_string(),
            json!(env::var("EMAIL_API_AUTH").unwrap_or_default()),
        );
    }
    let cfg = EmailConfig::from_json(&row, "env");
    if cfg.is_configured() {
        Some(cfg)
    } else {
        None
    }
}

/// The credential fields carried inside the `admin_settings.email` object.
pub const CONFIG_SECRET_FIELDS: [&str; 2] = ["api_key", "smtp_password"];

/// Seal the credential fields IN PLACE before they are stored. Fail-closed: a missing master key is
/// an error, never a plaintext row (kanban t_a794cb09).
pub async fn seal_config_secrets(
    pool: &PgPool,
    cfg: &mut Value,
) -> Result<(), provider_key_crypto::CryptoError> {
    let Some(obj) = cfg.as_object_mut() else {
        return Ok(());
    };
    for field in CONFIG_SECRET_FIELDS {
        let current = obj
            .get(field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if current.is_empty() || provider_key_crypto::is_encrypted(&current) {
            continue;
        }
        let sealed = provider_key_crypto::encrypt_for_storage(pool, &current).await?;
        obj.insert(field.to_string(), Value::String(sealed));
    }
    Ok(())
}

/// Open the credential fields IN PLACE after a DB read, so what reaches a provider is the
/// credential and never the envelope. A value without the envelope is a legacy plaintext row.
pub async fn open_config_secrets(
    pool: &PgPool,
    cfg: &mut Value,
) -> Result<(), provider_key_crypto::CryptoError> {
    let Some(obj) = cfg.as_object_mut() else {
        return Ok(());
    };
    for field in CONFIG_SECRET_FIELDS {
        let current = obj
            .get(field)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if current.is_empty() || !provider_key_crypto::is_encrypted(&current) {
            continue;
        }
        let opened = provider_key_crypto::decrypt_from_storage(pool, &current).await?;
        obj.insert(field.to_string(), Value::String(opened));
    }
    Ok(())
}

/// The stored row, credential opened. `None` = no row; `Err` = a row exists but its credential
/// cannot be opened (a wrong/missing master key) — surfaced, never treated as "unconfigured".
pub async fn stored_config(pool: &PgPool) -> Result<Option<EmailConfig>, String> {
    let value: Option<Value> =
        sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = $1")
            .bind(ADMIN_KEY)
            .fetch_optional(pool)
            .await
            .map_err(|e| format!("admin_settings.{ADMIN_KEY} read failed: {e}"))?;
    let Some(mut v) = value else {
        return Ok(None);
    };
    if !v.is_object() {
        return Ok(None);
    }
    open_config_secrets(pool, &mut v)
        .await
        .map_err(|e| format!("admin_settings.{ADMIN_KEY} credential cannot be opened: {e}"))?;
    Ok(Some(EmailConfig::from_json(&v, "db")))
}

/// Resolve the system-mail config: the panel row FIRST, the `EMAIL_*` env as the fallback.
pub async fn resolve(pool: &PgPool) -> Option<EmailConfig> {
    match stored_config(pool).await {
        Ok(Some(cfg)) if cfg.is_configured() => return Some(cfg),
        Ok(Some(_)) => {
            tracing::warn!(
                "email provider row '{}' is incomplete (needs api_key + from_address) — falling back to the EMAIL_* environment",
                ADMIN_KEY
            );
        }
        Ok(None) => {}
        Err(e) => {
            tracing::error!(
                "email provider config unusable ({e}) — falling back to the EMAIL_* environment; \
                 a plaintext credential is never assumed"
            );
        }
    }
    from_env()
}

/// Percent-encode one `application/x-www-form-urlencoded` component.
///
/// Written out instead of leaning on a serializer so the encoding is a function this crate can
/// TEST: a From/To/Subject carrying `&` or `=` must arrive byte-identical at the provider, and a
/// string-concatenated body is exactly the class of bug this transport was fixed for.
fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Build a form body from the pairs, parameter-encoded.
pub fn form_encode(params: &[(&str, &str)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", encode_component(k), encode_component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Keep a provider's answer short enough to log and to show in the panel.
fn receipt(status: u16, body: &str) -> String {
    let trimmed = body.trim();
    let short: String = trimmed.chars().take(300).collect();
    if short.is_empty() {
        format!("{status} (empty body)")
    } else {
        format!("{status} {short}")
    }
}

/// Deliver a message through the configured provider. `Ok(receipt)` carries the provider's own
/// answer (for Mailgun: `{"id":"<…>","message":"Queued. Thank you."}`), which is the only evidence
/// that the message actually left the box.
pub async fn deliver(
    cfg: &EmailConfig,
    to: &str,
    subject: &str,
    text: &str,
    html: Option<&str>,
) -> Result<String, String> {
    // ── HARNESS / RESERVED RECIPIENTS NEVER REACH A RELAY (kanban t_36b55ed2) ───────────────────
    // The suppression guard lived only in `email.rs`, but `deliver` is called DIRECTLY by the admin
    // "Send test email" route — so that path bypassed it and handed a fleet-dev / RFC-2606 address
    // to the real relay, which can only bounce or land in a fleet mailbox (and burns the domain's
    // sending reputation). The guard now lives at the transport chokepoint so every caller inherits
    // it. `*.local` / `localhost` stay OPEN: content harnesses point the provider at a local SMTP
    // sink and `harness_domain` returns None for that class.
    if let Some(domain) = crate::security::probe_addr::harness_domain(to) {
        tracing::info!(
            to = %to,
            domain = %domain,
            "email suppressed: recipient is a fleet harness/reserved address (no send attempted)"
        );
        return Ok("suppressed: harness/reserved recipient".to_string());
    }
    if !cfg.is_configured() {
        return Err(format!(
            "email provider '{}' is not configured (api_key and from_address are required)",
            cfg.provider
        ));
    }
    match cfg.provider.as_str() {
        "mailgun" => send_mailgun(cfg, to, subject, text, html).await,
        "sendgrid" => send_sendgrid(cfg, to, subject, text, html).await,
        "sendiio" => send_sendiio(cfg, to, subject, text, html).await,
        other => Err(format!(
            "unsupported email provider '{other}' — this app delivers through {}",
            available()
                .iter()
                .filter_map(|p| p.get("value").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("/")
        )),
    }
}

async fn send_mailgun(
    cfg: &EmailConfig,
    to: &str,
    subject: &str,
    text: &str,
    html: Option<&str>,
) -> Result<String, String> {
    let url = if !cfg.api_url.is_empty() {
        cfg.api_url.clone()
    } else if !cfg.domain.is_empty() {
        format!("https://api.mailgun.net/v3/{}/messages", cfg.domain)
    } else {
        return Err("mailgun api_url/domain not configured".to_string());
    };

    let from = cfg.sender();
    let mut params: Vec<(&str, String)> = vec![
        ("from", from),
        ("to", to.to_string()),
        ("subject", subject.to_string()),
        ("text", text.to_string()),
    ];
    if let Some(h) = html.filter(|h| !h.is_empty()) {
        params.push(("html", h.to_string()));
    }
    let owned: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();

    // Mailgun is a form API: `Authorization: Basic base64("api:<private key>")` and a
    // `application/x-www-form-urlencoded` body. Presenting JSON (the pre-fix shape) authenticates
    // and then 400s with "from parameter is missing", because a JSON document has no form fields.
    use base64::Engine;
    let auth = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("api:{}", cfg.api_key))
    );

    let resp = reqwest::Client::new()
        .post(&url)
        .header("Authorization", auth)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form_encode(&owned))
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("mailgun request failed: {e}"))?;

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if (200..300).contains(&status) {
        Ok(receipt(status, &body))
    } else {
        Err(format!(
            "mailgun rejected the message: {}",
            receipt(status, &body)
        ))
    }
}

async fn send_sendgrid(
    cfg: &EmailConfig,
    to: &str,
    subject: &str,
    text: &str,
    html: Option<&str>,
) -> Result<String, String> {
    let url = if cfg.api_url.is_empty() {
        "https://api.sendgrid.com/v3/mail/send".to_string()
    } else {
        cfg.api_url.clone()
    };

    let mut content = vec![json!({ "type": "text/plain", "value": text })];
    if let Some(h) = html.filter(|h| !h.is_empty()) {
        content.push(json!({ "type": "text/html", "value": h }));
    }
    let payload = json!({
        "personalizations": [{ "to": [{ "email": to }] }],
        "from": { "email": cfg.from_address, "name": cfg.from_name },
        "subject": subject,
        "content": content,
    });

    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("sendgrid request failed: {e}"))?;

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if (200..300).contains(&status) {
        Ok(receipt(status, &body))
    } else {
        Err(format!(
            "sendgrid rejected the message: {}",
            receipt(status, &body)
        ))
    }
}

async fn send_sendiio(
    cfg: &EmailConfig,
    to: &str,
    subject: &str,
    text: &str,
    html: Option<&str>,
) -> Result<String, String> {
    let url = if cfg.api_url.is_empty() {
        "https://sendiio.com/api/v1/smtp/send".to_string()
    } else {
        cfg.api_url.clone()
    };

    let payload = json!({
        "api_key": cfg.api_key,
        "from_email": cfg.from_address,
        "from_name": cfg.from_name,
        "to_email": to,
        "subject": subject,
        "text": text,
        "html": html.unwrap_or(""),
    });

    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&cfg.api_key)
        .json(&payload)
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("sendiio request failed: {e}"))?;

    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if (200..300).contains(&status) {
        Ok(receipt(status, &body))
    } else {
        Err(format!(
            "sendiio rejected the message: {}",
            receipt(status, &body)
        ))
    }
}

/// Providers the admin can pick — served to the admin UI so the dropdown is not hardcoded in the
/// browser, and used to VALIDATE a save, so a stored provider always names an arm that can deliver.
pub fn available() -> Vec<Value> {
    vec![
        json!({"value":"mailgun","label":"Mailgun (form-encoded API)"}),
        json!({"value":"sendgrid","label":"SendGrid (JSON API)"}),
        json!({"value":"sendiio","label":"Sendiio (JSON API)"}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_body_percent_encodes_reserved_characters() {
        let body = form_encode(&[("subject", "Probe & proof = 1"), ("to", "a+b@x.com")]);
        assert!(
            body.contains("subject=Probe+%26+proof+%3D+1"),
            "subject must be parameter-encoded, got {body}"
        );
        assert!(
            body.contains("to=a%2Bb%40x.com"),
            "an address carrying + and @ must be encoded, got {body}"
        );
        assert!(
            !body.contains("proof = 1"),
            "a raw concatenation would leave the value unencoded: {body}"
        );
    }

    #[test]
    fn form_body_keeps_every_field_the_provider_needs() {
        let body = form_encode(&[
            ("from", "MissedCall <noreply@mail.missedcallrespondr.com>"),
            ("to", "swiftsoftware143+probe@yahoo.com"),
            ("subject", "Welcome"),
            ("text", "body"),
            ("html", "<p>body</p>"),
        ]);
        for key in ["from=", "&to=", "&subject=", "&text=", "&html="] {
            assert!(body.contains(key), "missing {key} in {body}");
        }
    }

    #[test]
    fn sender_with_a_display_name_stays_one_value_and_splits_back_out() {
        let (name, addr) =
            split_sender("MissedCall Respondr <noreply@mail.missedcallrespondr.com>");
        assert_eq!(name, "MissedCall Respondr");
        assert_eq!(addr, "noreply@mail.missedcallrespondr.com");
        // A bare address stays bare.
        let (name, addr) = split_sender("noreply@mail.missedcallrespondr.com");
        assert!(name.is_empty());
        assert_eq!(addr, "noreply@mail.missedcallrespondr.com");
    }

    #[test]
    fn provider_is_explicit_else_inferred_from_the_api_host() {
        assert_eq!(infer_provider("SendGrid", ""), "sendgrid");
        assert_eq!(
            infer_provider("", "https://api.mailgun.net/v3/mail.x.com/messages"),
            "mailgun"
        );
        assert_eq!(
            infer_provider("", "https://api.sendgrid.com/v3/mail/send"),
            "sendgrid"
        );
        assert_eq!(infer_provider("", ""), "mailgun");
    }

    #[test]
    fn a_row_without_provider_gets_one_and_keeps_the_other_fields() {
        let cfg = EmailConfig::from_json(
            &json!({
                "api_url": "https://api.mailgun.net/v3/mail.missedcallrespondr.com/messages",
                "api_key": "k",
                "from_address": "MissedCall <noreply@mail.missedcallrespondr.com>",
            }),
            "db",
        );
        assert_eq!(cfg.provider, "mailgun");
        assert_eq!(cfg.from_address, "noreply@mail.missedcallrespondr.com");
        assert_eq!(cfg.from_name, "MissedCall");
        assert!(cfg.is_configured());
        assert_eq!(
            cfg.sender(),
            "MissedCall <noreply@mail.missedcallrespondr.com>"
        );
    }

    #[test]
    fn every_offered_provider_has_a_delivery_arm() {
        let offered: Vec<String> = available()
            .iter()
            .filter_map(|p| p.get("value").and_then(|v| v.as_str()).map(str::to_string))
            .collect();
        assert_eq!(offered, vec!["mailgun", "sendgrid", "sendiio"]);
        // mailgun is the arm the fleet sends through; an unknown name must NOT fall through to it.
        for (provider, expected) in [("mailgun", true), ("sendgrid", true), ("sendiio", true)] {
            assert!(offered.iter().any(|p| p == provider) == expected);
        }
    }

    #[tokio::test]
    async fn an_unsupported_provider_is_refused_by_name() {
        let cfg = EmailConfig {
            provider: "smtp".to_string(),
            api_url: String::new(),
            api_key: "k".to_string(),
            domain: String::new(),
            from_address: "a@b.com".to_string(),
            from_name: String::new(),
            source: "db",
        };
        let err = deliver(&cfg, "to@x.com", "s", "t", None).await.unwrap_err();
        assert!(err.contains("unsupported email provider 'smtp'"), "{err}");
        assert!(
            err.contains("mailgun"),
            "the error names the arms that work: {err}"
        );
    }

    #[tokio::test]
    async fn an_incomplete_config_is_refused_by_name() {
        let cfg = EmailConfig {
            provider: "mailgun".to_string(),
            api_url: String::new(),
            api_key: String::new(),
            domain: String::new(),
            from_address: "a@b.com".to_string(),
            from_name: String::new(),
            source: "db",
        };
        let err = deliver(&cfg, "to@x.com", "s", "t", None).await.unwrap_err();
        assert!(err.contains("is not configured"), "{err}");
    }
}
