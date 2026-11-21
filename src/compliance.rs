//! Email compliance — a do-not-mail list and a working one-click opt-out for this app's
//! notification mail (2026-11-21, port of the IncentiveSwift / WorkflowSwift / FunnelSwift shape).
//!
//! What this app needed and did not have: a place to record "this person asked us to stop", an
//! unsubscribe link that actually lands somewhere, and a gate in the send path that honours both.
//! MissedCall Respondr had no place to record "stop mailing this address" and no link that landed
//! anywhere, so an address that asked to stop — or one the operator watched hard-bounce — kept
//! receiving mail. That is the fastest way to damage the sending domain, a shared asset for every
//! customer on it. This is the same layer the four sibling apps carry (FunnelSwift 139, ADASwift
//! 037, IncentiveSwift, WorkflowSwift): a do-not-mail list, a one-click opt-out that records a
//! refusal, a gate in the send path that honours both, and an operator screen for the list.
//!
//! Two classes, exactly as the rest of the fleet defines them:
//!
//!  * `transactional` — credentials, password resets, receipts, security notices. Exempt from opt-out
//!    by law and NOT gated here: somebody who unsubscribed from notices must still be able to sign in.
//!  * `notification` — storage warnings, and any notice this app chooses to send. Gated, and every
//!    one carries the opt-out footer.
//!
//! Suppression is scoped to the ACCOUNT (tenant), never global: one workspace's opt-out must never
//! silence another workspace's mail to the same person. System-wide mail (a prospect who was never a
//! tenant) is recorded under the all-zero tenant uuid, which is this crate's own "no tenant"
//! sentinel, so an outreach opt-out also stops the next outreach message.
//!
//! The gate FAILS OPEN. A database error while reading the list must never stop mail that would
//! otherwise be legitimate — the failure is logged and the send proceeds, because a silent mailing
//! freeze is worse than one extra message.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

/// Mail that may be stopped by an opt-out.
pub const NOTIFICATION: &str = "notification";
/// Mail that may not (credentials, resets, receipts, security notices).
pub const TRANSACTIONAL: &str = "transactional";

// Where the customer-facing pages live for this app — taken from the app's own constant so the
// opt-out link can never point at a host this install does not serve.
use crate::branding::APP_URL;

/// The template types that count as notification mail. Everything else is transactional, so a
/// template added later is safe by default — it is never silently stoppable.
///
/// Deliberately NOT here: `welcome` / `account_ready` (they carry credentials), `password_reset`,
/// `email_change`, `purchase_confirmed` (a receipt), `widget_delivery` (the paid deliverable),
/// `portfolio_provision`, `branding_preview`, `support_request_receipt` (the answer to the person's
/// own request).
const NOTIFICATION_TEMPLATES: &[&str] = &[
    // The one notice this app CHOOSES to send rather than one a customer's own action requires: a
    // storage warning on their private mailbox. Everything else it mails is transactional by nature
    // — credentials, password resets, receipts, support replies, plan/billing notices about the
    // person's own account — and stays unstoppable, because withholding it would lock someone out.
    // A template added later is therefore transactional unless it is named here.
    "storage_warning",
];

/// Classify a template type. Anything not named above is transactional.
pub fn class_of(template_type: &str) -> &'static str {
    if NOTIFICATION_TEMPLATES.contains(&template_type) {
        NOTIFICATION
    } else {
        TRANSACTIONAL
    }
}

pub fn normalise(address: &str) -> String {
    address.trim().to_lowercase()
}

/// The signing secret for opt-out tokens. It is derived from a value that already lives in the
/// deployment's environment, so a token cannot be forged without the app's own secret, and no new
/// secret has to be handled by the operator.
fn secret() -> String {
    std::env::var("UNSUBSCRIBE_SECRET")
        .or_else(|_| std::env::var("JWT_SECRET"))
        .unwrap_or_default()
}

fn sign(key: &str, payload: &str) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.as_bytes())
        .expect("hmac accepts a key of any length");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// A tamper-proof opt-out token: account + address + class. No expiry — a link in a year-old
/// message must still work, and there is no state to lose.
pub fn token_for(key: &str, tenant_id: Uuid, address: &str, mail_class: &str) -> String {
    let addr = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(normalise(address));
    let payload = format!("{tenant_id}.{addr}.{mail_class}");
    let sig = sign(key, &payload);
    format!("{payload}.{sig}")
}

pub fn verify_token(key: &str, token: &str) -> Option<(Uuid, String, String)> {
    if key.is_empty() {
        return None;
    }
    let (payload, sig) = token.rsplit_once('.')?;
    if sign(key, payload) != sig {
        return None;
    }
    let mut it = payload.split('.');
    let tenant_id: Uuid = it.next()?.parse().ok()?;
    let address = String::from_utf8(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(it.next()?)
            .ok()?,
    )
    .ok()?;
    let mail_class = it.next()?.to_string();
    Some((tenant_id, address, mail_class))
}

/// The link a notification carries. It is a real page, not a mailbox.
pub fn unsubscribe_url(tenant_id: Uuid, address: &str, mail_class: &str) -> Option<String> {
    let key = secret();
    if key.is_empty() {
        return None;
    }
    Some(format!(
        "{APP_URL}/api/v1/public/unsubscribe/{}",
        token_for(&key, tenant_id, address, mail_class)
    ))
}

/// The suppression reasons that block `mail_class`. `all` blocks every class that can be blocked.
fn blocking_classes(mail_class: &str) -> &'static [&'static str] {
    match mail_class {
        TRANSACTIONAL => &["all"],
        _ => &["all", "notification"],
    }
}

/// Has this account been asked not to mail this address in this class? Returns the reason when so.
pub async fn suppression(
    db: &PgPool,
    tenant_id: Uuid,
    address: &str,
    mail_class: &str,
) -> Option<String> {
    let classes: Vec<String> = blocking_classes(mail_class)
        .iter()
        .map(|c| c.to_string())
        .collect();
    sqlx::query_scalar::<_, String>(
        "SELECT reason FROM email_suppressions
          WHERE tenant_id = $1 AND address = $2 AND mail_class = ANY($3)
          ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(normalise(address))
    .bind(&classes)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// THE GATE. `None` means send; `Some(reason)` means withhold.
///
/// Fails open by construction: a read error is logged and answered `None`, so a database problem
/// can never turn into a silent stop of every message the app sends.
pub async fn gate(db: &PgPool, tenant_id: Uuid, address: &str, mail_class: &str) -> Option<String> {
    let classes: Vec<String> = blocking_classes(mail_class)
        .iter()
        .map(|c| c.to_string())
        .collect();
    match sqlx::query_scalar::<_, String>(
        "SELECT reason FROM email_suppressions
          WHERE tenant_id = $1 AND address = $2 AND mail_class = ANY($3)
          ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(normalise(address))
    .bind(&classes)
    .fetch_optional(db)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(to = %address, "suppression gate could not read the do-not-mail list — sending anyway: {e}");
            None
        }
    }
}

/// Record an opt-out. Idempotent: a second click does not create a second row.
pub async fn suppress(
    db: &PgPool,
    tenant_id: Uuid,
    address: &str,
    mail_class: &str,
    reason: &str,
    source: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO email_suppressions (tenant_id, address, mail_class, reason, source)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (tenant_id, address, mail_class) DO UPDATE
            SET reason = EXCLUDED.reason, source = EXCLUDED.source, updated_at = now()",
    )
    .bind(tenant_id)
    .bind(normalise(address))
    .bind(mail_class)
    .bind(reason)
    .bind(source)
    .execute(db)
    .await?;
    Ok(())
}

/// A footer on every notification mail: the plain-text line and the HTML line, each carrying a
/// working link for THAT recipient. Added only to notification mail.
pub fn with_unsubscribe_footer(
    tenant_id: Uuid,
    address: &str,
    text: Option<String>,
    html: Option<String>,
) -> (Option<String>, Option<String>) {
    let Some(url) = unsubscribe_url(tenant_id, address, NOTIFICATION) else {
        return (text, html);
    };
    let text = text.map(|t| {
        if t.contains("/api/v1/public/unsubscribe/") {
            t
        } else {
            format!("{t}\n\nDon't want these notices? Unsubscribe: {url}\n")
        }
    });
    let html = html.map(|h| {
        if h.contains("/api/v1/public/unsubscribe/") {
            h
        } else {
            format!(
                "{h}\n<p style=\"font-size:13px;color:#6b7280;text-align:center;\">Don't want these notices? <a href=\"{url}\">Unsubscribe</a>.</p>"
            )
        }
    });
    (text, html)
}

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title} — MissedCall Respondr</title>\
         <style>body{{margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;\
         font-family:system-ui,-apple-system,Segoe UI,sans-serif;background:#0b1220;color:#e5e7eb}}\n\
         main{{max-width:34rem;padding:2rem;text-align:center}}h1{{font-size:1.5rem;margin:0 0 1rem}}\n\
         p{{line-height:1.6;color:#9ca3af}}</style></head><body><main>\
         <h1>{title}</h1><p>{body}</p></main></body></html>"
    ))
}

fn refusal(status: StatusCode, message: &str) -> Response {
    let mut res = page("That link did not work", message).into_response();
    *res.status_mut() = status;
    res
}

/// `GET /api/v1/public/unsubscribe/:token` — the page the link in a message opens.
///
/// It records the opt-out on GET because that is what the recipient actually does (click the link),
/// and it is idempotent so a mail scanner that pre-fetches the link cannot damage anything beyond
/// unsubscribing the person it was addressed to.
pub async fn show_unsubscribe(
    State(state): State<crate::state::AppState>,
    Path(token): Path<String>,
) -> Response {
    let key = secret();
    let Some((tenant_id, address, mail_class)) = verify_token(&key, &token) else {
        return refusal(
            StatusCode::BAD_REQUEST,
            "This unsubscribe link is not valid. It may have been cut short by your mail app — \
             copy the whole link, or reply to the message and we will stop the mail for you.",
        );
    };
    match suppress(
        &state.pool,
        tenant_id,
        &address,
        &mail_class,
        "recipient unsubscribed",
        "unsubscribe-link",
    )
    .await
    {
        Ok(()) => page(
            "You're unsubscribed",
            "We will not send you any more notices from this account. Security and account emails \
             still reach you, because you may need them to sign in.",
        )
        .into_response(),
        Err(e) => {
            tracing::warn!("unsubscribe could not be recorded: {e}");
            refusal(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Something went wrong recording your request. Please reply to the message and we \
                 will stop the mail for you.",
            )
        }
    }
}

/// `POST /api/v1/public/unsubscribe/:token` — the RFC 8058 one-click form a mail client calls. Same
/// effect, no page.
pub async fn one_click(
    State(state): State<crate::state::AppState>,
    Path(token): Path<String>,
) -> Response {
    let key = secret();
    let Some((tenant_id, address, mail_class)) = verify_token(&key, &token) else {
        return (StatusCode::BAD_REQUEST, "invalid unsubscribe token").into_response();
    };
    match suppress(
        &state.pool,
        tenant_id,
        &address,
        &mail_class,
        "recipient unsubscribed",
        "one-click",
    )
    .await
    {
        Ok(()) => (StatusCode::OK, "unsubscribed").into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "could not record").into_response(),
    }
}

// ── the operator's own view of the list (2026-11-21) ─────────────────────────────────────────────
//
// The public page records a refusal; without a screen the operator could not SEE who asked to stop,
// could not add an address that asked by phone or that keeps bouncing, could not allow one again,
// and could not keep the list as a file. FunnelSwift shipped exactly this surface (commit a41d699,
// ported from IncentiveSwift); this is the same four doors, and every one of them is scoped to the
// CALLER's own workspace, so one account can never read or edit another's list. A foreign id answers
// a plain 404 rather than a 403 that would confirm the row exists.

/// One row of the operator's do-not-mail list.
#[derive(serde::Serialize, sqlx::FromRow)]
pub struct SuppressionRow {
    pub id: Uuid,
    pub address: String,
    pub mail_class: String,
    pub reason: String,
    pub source: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// The workspace the signed-in caller acts in. It comes from the token's own `aid` claim (a `Uuid`
/// in this app) — never from the path or the body, which is what keeps every route below scoped to
/// the caller.
fn caller_tenant(claims: &crate::config::Claims) -> Option<Uuid> {
    Some(claims.aid)
}

/// The answer when the credential names no workspace. A token with an unparseable tenant is a
/// credential fault, not a server fault, so it is a 400 with a sentence rather than a panic.
fn no_tenant() -> Response {
    (StatusCode::BAD_REQUEST, "invalid tenant in the credential").into_response()
}

/// `GET /api/v1/settings/suppressions` — who has asked this workspace to stop mailing them.
pub async fn list_suppressions(
    State(state): State<crate::state::AppState>,
    axum::Extension(claims): axum::Extension<crate::config::Claims>,
) -> Response {
    let Some(tenant_id) = caller_tenant(&claims) else {
        return no_tenant();
    };
    match sqlx::query_as::<_, SuppressionRow>(
        "SELECT id, address, mail_class, reason, source, created_at
           FROM email_suppressions WHERE tenant_id = $1
          ORDER BY created_at DESC LIMIT 500",
    )
    .bind(tenant_id)
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => axum::Json(serde_json::json!({ "suppressions": rows })).into_response(),
        Err(e) => {
            tracing::warn!("could not list suppressions: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "could not read the list").into_response()
        }
    }
}

/// `POST /api/v1/settings/suppressions` — add an address by hand (a request made by phone, or an
/// address the operator watched hard-bounce). `mail_class` is `all` for a blanket stop and
/// `notification` for notice mail only.
pub async fn add_suppression(
    State(state): State<crate::state::AppState>,
    axum::Extension(claims): axum::Extension<crate::config::Claims>,
    axum::Json(body): axum::Json<serde_json::Value>,
) -> Response {
    let Some(tenant_id) = caller_tenant(&claims) else {
        return no_tenant();
    };
    let address = body
        .get("address")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !address.contains('@') || address.len() < 5 || address.len() > 254 {
        return (StatusCode::BAD_REQUEST, "a full email address is required").into_response();
    }
    let mail_class = match body.get("mail_class").and_then(|v| v.as_str()) {
        Some("all") => "all",
        Some(NOTIFICATION) | None => NOTIFICATION,
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                "mail_class is 'all' or 'notification'",
            )
                .into_response()
        }
    };
    let reason = body
        .get("reason")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "added by the operator".to_string());
    match suppress(
        &state.pool,
        tenant_id,
        &address,
        mail_class,
        &reason,
        "operator",
    )
    .await
    {
        Ok(()) => axum::Json(serde_json::json!({
            "address": normalise(&address), "mail_class": mail_class, "suppressed": true
        }))
        .into_response(),
        Err(e) => {
            tracing::warn!("could not record a suppression: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "could not record").into_response()
        }
    }
}

/// `DELETE /api/v1/settings/suppressions/:id` — allow one address again (a wrong address, or a person
/// who asked to start receiving notices). Scoped by tenant, so an id from another workspace answers a
/// plain 404 instead of a 403 that would confirm the row exists.
pub async fn remove_suppression(
    State(state): State<crate::state::AppState>,
    axum::Extension(claims): axum::Extension<crate::config::Claims>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(tenant_id) = caller_tenant(&claims) else {
        return no_tenant();
    };
    match sqlx::query("DELETE FROM email_suppressions WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(&state.pool)
        .await
    {
        Ok(r) if r.rows_affected() == 1 => StatusCode::NO_CONTENT.into_response(),
        Ok(_) => (StatusCode::NOT_FOUND, "no such entry").into_response(),
        Err(e) => {
            tracing::warn!("could not remove a suppression: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "could not remove").into_response()
        }
    }
}

/// One CSV row. Every field is quoted and an inner quote is doubled, so a reason containing a comma
/// or a quote cannot break the row structure a spreadsheet reads.
fn csv_row(r: &SuppressionRow) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    format!(
        "{},{},{},{},{}\n",
        q(&r.address),
        q(&r.mail_class),
        q(&r.reason),
        q(&r.source),
        q(&r.created_at.to_rfc3339())
    )
}

/// `GET /api/v1/settings/suppressions.csv` — the same list as a file the operator can keep.
pub async fn suppressions_csv(
    State(state): State<crate::state::AppState>,
    axum::Extension(claims): axum::Extension<crate::config::Claims>,
) -> Response {
    let Some(tenant_id) = caller_tenant(&claims) else {
        return no_tenant();
    };
    let rows = sqlx::query_as::<_, SuppressionRow>(
        "SELECT id, address, mail_class, reason, source, created_at
           FROM email_suppressions WHERE tenant_id = $1
          ORDER BY created_at DESC LIMIT 5000",
    )
    .bind(tenant_id)
    .fetch_all(&state.pool)
    .await;
    let Ok(rows) = rows else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not read the list").into_response();
    };
    let mut out = String::from("address,class,reason,source,added\n");
    for r in &rows {
        out.push_str(&csv_row(r));
    }
    (
        StatusCode::OK,
        [
            ("content-type", "text/csv; charset=utf-8"),
            (
                "content-disposition",
                "attachment; filename=\"do-not-mail.csv\"",
            ),
        ],
        out,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Uuid = Uuid::from_u128(7);

    #[test]
    fn a_token_round_trips_and_a_tampered_one_does_not() {
        let key = "test-secret";
        let tok = token_for(key, T, "Person@Example.com", NOTIFICATION);
        let (t, addr, class) = verify_token(key, &tok).expect("valid token");
        assert_eq!(t, T);
        assert_eq!(addr, "person@example.com");
        assert_eq!(class, NOTIFICATION);
        let mut bad = tok.clone();
        bad.pop();
        assert!(verify_token(key, &format!("{bad}0")).is_none());
        assert!(verify_token("other-secret", &tok).is_none());
    }

    #[test]
    fn an_empty_secret_verifies_nothing() {
        let tok = token_for("k", T, "a@b.com", NOTIFICATION);
        assert!(verify_token("", &tok).is_none());
    }

    #[test]
    fn classes_are_safe_by_default() {
        // The one notice this app chooses to send is stoppable...
        assert_eq!(class_of("storage_warning"), NOTIFICATION);
        // ...and everything a customer's own action depends on stays unstoppable: credentials,
        // resets, receipts, replies and plan/billing notices about their own account, plus any
        // template added later that nobody has classified.
        assert_eq!(class_of("welcome"), TRANSACTIONAL);
        assert_eq!(class_of("welcome_credentials"), TRANSACTIONAL);
        assert_eq!(class_of("password_reset"), TRANSACTIONAL);
        assert_eq!(class_of("support_request_receipt"), TRANSACTIONAL);
        assert_eq!(class_of("ticket_reply"), TRANSACTIONAL);
        assert_eq!(class_of("purchase_confirmed"), TRANSACTIONAL);
        assert_eq!(class_of("plan_changed"), TRANSACTIONAL);
        assert_eq!(class_of("something_invented_next_year"), TRANSACTIONAL);
    }

    #[test]
    fn the_opt_out_link_points_at_the_apps_own_host() {
        std::env::set_var("UNSUBSCRIBE_SECRET", "unsubscribe-test-secret");
        let url = unsubscribe_url(T, "a@b.com", NOTIFICATION).expect("url");
        assert!(
            url.starts_with("https://app.missedcallrespondr.com/api/v1/public/unsubscribe/"),
            "{url}"
        );
    }

    #[test]
    fn the_csv_row_quotes_every_field_so_a_comma_cannot_break_it() {
        let row = SuppressionRow {
            id: T,
            address: "a@b.com".to_string(),
            mail_class: NOTIFICATION.to_string(),
            reason: "asked by phone, then said \"stop\"".to_string(),
            source: "operator".to_string(),
            created_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };
        let line = csv_row(&row);
        assert_eq!(
            line,
            "\"a@b.com\",\"notification\",\"asked by phone, then said \"\"stop\"\"\",\"operator\",\"2023-11-14T22:13:20+00:00\"\n"
        );
    }

    #[test]
    fn the_footer_only_applies_to_notification_mail_and_carries_the_link() {
        std::env::set_var("UNSUBSCRIBE_SECRET", "unsubscribe-test-secret");
        let (t, h) = with_unsubscribe_footer(
            T,
            "a@b.com",
            Some("body".to_string()),
            Some("<p>body</p>".to_string()),
        );
        let t = t.expect("text");
        let h = h.expect("html");
        assert!(t.contains("/api/v1/public/unsubscribe/"), "{t}");
        assert!(h.contains("/api/v1/public/unsubscribe/"), "{h}");
        // idempotent — a second pass does not add a second footer
        let (t2, _) = with_unsubscribe_footer(T, "a@b.com", Some(t.clone()), None);
        assert_eq!(t2.expect("text").matches("Unsubscribe:").count(), 1);
    }
}
