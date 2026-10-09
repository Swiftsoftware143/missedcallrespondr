use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::email_provider;

use crate::branding;

/// Render a template string by substituting the placeholders that ARE keys of `vars`.
///
/// Two brace conventions are accepted because both are live: every shipped row — and anything an
/// admin can write from the console today, since the Create/Update form offers no placeholder help
/// and no validation — uses a SINGLE brace (`Welcome to {app_name}!`), while this function only
/// ever understood `{{key}}`. So a db template that WAS selected emitted its placeholders
/// literally: the recipient got `Your password reset code is: {token}` (card t_215941a2, measured
/// from the sender's own log line on the live welcome row).
///
/// The substitution is KEY-DRIVEN, not a regex sweep: only `{k}` / `{{k}}` for a `k` present in
/// `vars` is replaced, so a template holding a real brace that is not a placeholder — JSON in a
/// body, a CSS block — survives byte-for-byte. A key that is NOT in `vars` is likewise left
/// literal, deliberately: an unfilled placeholder must stay visible rather than render as "".
fn render_template(template: &str, vars: &serde_json::Value) -> String {
    let mut result = template.to_string();

    if let Some(obj) = vars.as_object() {
        for (key, value) in obj {
            let replacement = value.as_str().unwrap_or("");
            // `{{key}}` FIRST: replacing the single form first would eat the inner braces of a
            // `{{key}}` placeholder and leave a stray `{}`-wrapped value behind. With this order
            // both conventions work, which matters because the double form is the one the older
            // doc comment (and the audit harness) wrote.
            let placeholder = format!("{{{{{}}}}}", key);
            result = result.replace(&placeholder, replacement);
            result = result.replace(&format!("{{{}}}", key), replacement);
        }
    }

    result
}

/// The values every template may rely on whichever entry point sent it.
///
/// Before this, `{app_name}` and `{login_url}` resolved only on the call sites that happened to
/// pass them — `auth::register` passed both, the checkout welcome path passed neither — so the
/// SAME live template rendered differently per entry point (and rendered LITERALLY wherever the
/// key was missing, which is what the welcome row's subject showed). App-level facts are merged in
/// as defaults; caller-supplied keys win, so a caller that knows a better value still overrides.
///
/// `login_url` is the app base URL, not `<base>/login`: the shipped row writes
/// `Login: {login_url}/login`, and `auth::register` already passes the bare base for it.
fn with_app_vars(vars: &serde_json::Value, app_name: &str, app_url: &str) -> serde_json::Value {
    let mut merged = serde_json::Map::new();
    merged.insert("app_name".to_string(), json!(app_name));
    merged.insert("app_url".to_string(), json!(app_url));
    merged.insert("login_url".to_string(), json!(app_url));
    if let Some(obj) = vars.as_object() {
        for (key, value) in obj {
            merged.insert(key.clone(), value.clone());
        }
    }
    serde_json::Value::Object(merged)
}

/// Bind the two branding merge fields into a render's variable map (kanban t_feab8aff).
///
/// They are PER-ACCOUNT, so they cannot live in a `&'static` default map: the account's own values
/// when it has branding, the app's identity otherwise — so an admin-authored `{brand_name}` never
/// reaches a recipient as literal text. Every other key of the map is left alone.
fn bind_branding(vars: &mut serde_json::Value, branding: Option<&branding::Branding>) {
    if !vars.is_object() {
        *vars = json!({});
    }
    let (name, logo) = match branding {
        Some(b) => (
            b.brand_name.clone(),
            b.resolve_logo_url(branding::APP_URL).unwrap_or_default(),
        ),
        None => (branding::APP_NAME.to_string(), String::new()),
    };
    if let Some(obj) = vars.as_object_mut() {
        obj.insert("brand_name".to_string(), json!(name));
        obj.insert("logo_url".to_string(), json!(logo));
    }
}

/// Put the account's branding at the TOP of a rendered message (kanban t_feab8aff).
///
/// The HTML part gains the header block (logo + name + colour rule); the text part gains the brand
/// name as a one-line header (a text part cannot carry an image). When the account HAS branding but
/// the message carries no HTML part — the inline fallbacks return text only — one is built from the
/// ESCAPED text so a branded mail still shows the logo. A no-op for an account with no branding,
/// which is what makes the change additive: those renders are byte-identical to before.
fn apply_branding(
    branding: Option<&branding::Branding>,
    text: &str,
    html: &str,
) -> (String, String) {
    let Some(b) = branding else {
        return (text.to_string(), html.to_string());
    };
    let logo = b.resolve_logo_url(branding::APP_URL);
    let header = b.header_html(logo.as_deref());
    let html_out = if html.is_empty() {
        format!(
            "{header}<div style=\"white-space:pre-wrap;font:14px/1.5 -apple-system,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;color:#111827\">{}</div>",
            branding::escape_html(text)
        )
    } else {
        format!("{header}{html}")
    };
    let text_out = format!("{}{}", b.text_header(), text);
    (text_out, html_out)
}

/// Send a templated email using database-stored templates.
/// Falls back to old inline methods when no template found.
pub async fn send_template_email(
    pool: &PgPool,
    tenant_id: Uuid,
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
) -> Result<(), String> {
    let app_name = "MissedCall Respondr";
    let app_url = "https://app.missedcallrespondr.com";

    // A template must render against the SAME values no matter which entry point triggered the
    // send, so the app-level facts are merged in here rather than left to each call site.
    let mut vars = with_app_vars(vars, app_name, app_url);
    // Per-account branding merge fields (kanban t_feab8aff), loaded at the ONE funnel so a template
    // added later inherits them for free. The header block is applied to the rendered parts in
    // `send_email_request`, the choke point both this arm and the inline fallbacks pass through.
    let branding = branding::load(pool, tenant_id).await;
    bind_branding(&mut vars, branding.as_ref());

    // Load template from DB. A lookup FAILURE is no longer swallowed: `lookup_db_template`
    // logs it with this function's name and the error, then the caller falls back to the
    // inline body — so "no template configured" and "the query failed" stop looking alike.
    let template = lookup_db_template(pool, tenant_id, template_type).await;

    match template {
        Some(t) => {
            let subject = render_template(
                &t.subject
                    .clone()
                    .unwrap_or_else(|| get_default_subject(template_type, app_name)),
                &vars,
            );
            tracing::info!(
                "email.send_template_email: using db template {template_type} (id={}, name={:?}) subject={subject:?}",
                t.id,
                t.name
            );
            let html_body = t
                .html_body
                .as_ref()
                .map(|h| render_template(h, &vars))
                .unwrap_or_default();
            let text_body = render_template(&t.body.clone().unwrap_or_default(), &vars);
            let use_html = t.is_html.unwrap_or(true);

            send_email_request(
                pool,
                tenant_id,
                template_type,
                to,
                &subject,
                &text_body,
                if use_html { &html_body } else { "" },
            )
            .await
        }
        None => {
            tracing::info!(
                "email.send_template_email: no usable db template for template_type={template_type} (tenant {tenant_id}) — sending the inline body"
            );
            send_inline(pool, tenant_id, to, template_type, &vars, app_name, app_url).await
        }
    }
}

/// Fetch the tenant's template (or the global default) for `template_type`.
///
/// Returns `None` both when there is no template row and when the query failed — but a
/// failure is logged first, which is the whole point: previously this was
/// `.fetch_optional(..).await.ok().flatten()`, so a query that never ran (a missing column,
/// a lock, a connection error) was indistinguishable from "nothing configured", and
/// `send_template_email` fell back to the inline body without a word. That silent fallback
/// is what hid this app's `email_templates.is_html` schema drift (card t_99365fd5); the
/// column now exists (migration 000017), and if it ever drifts again this logs it.
pub async fn lookup_db_template(
    pool: &PgPool,
    tenant_id: Uuid,
    template_type: &str,
) -> Option<EmailTemplateRow> {
    match sqlx::query_as::<_, EmailTemplateRow>(
        r#"
        SELECT id, name, subject, body, html_body, is_html, is_default
        FROM email_templates
        WHERE template_type = $1 AND (aid = $2 OR is_default = true)
        ORDER BY is_default ASC, created_at DESC
        LIMIT 1
        "#,
    )
    .bind(template_type)
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(
                "email.lookup_db_template: query failed (template_type={template_type}, tenant_id={tenant_id}): {e} — falling back to the inline body"
            );
            None
        }
    }
}

fn get_default_subject(template_type: &str, app_name: &str) -> String {
    match template_type {
        // `welcome_credentials` is the welcome mail for flows that GENERATE the password (checkout):
        // same wording, but its row carries the credentials block (card t_46d8d40e).
        "welcome" | "welcome_credentials" => format!("Welcome to {}!", app_name),
        "purchase_confirmed" => "Payment Received — Thank You!".to_string(),
        "password_reset" => "Password Reset Request".to_string(),
        _ => format!("{} Notification", app_name),
    }
}

async fn send_inline(
    pool: &PgPool,
    tenant_id: Uuid,
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
    app_name: &str,
    app_url: &str,
) -> Result<(), String> {
    let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
    let email = vars.get("email").and_then(|v| v.as_str()).unwrap_or("");
    let password = vars.get("password").and_then(|v| v.as_str()).unwrap_or("");
    let token = vars.get("token").and_then(|v| v.as_str()).unwrap_or("");
    let plan_name_val = vars
        .get("plan_name")
        .and_then(|v| v.as_str())
        .unwrap_or("a plan");

    match template_type {
        // Same body for both welcome types: `welcome_credentials` exists so the CREDENTIALS row can
        // be a distinct, editable template, but if that row is missing the generated password must
        // still reach the customer, so the fallback keeps the credentials block (t_46d8d40e). The
        // self-serve `welcome` fallback therefore renders `Password: ` empty — pre-existing
        // behaviour, unchanged here, and it leaks nothing (the live default row always exists).
        "welcome" | "welcome_credentials" => {
            let body = format!(
                "Welcome to {}, {}!\n\nYour account has been created successfully.\n\nHere are your login credentials:\n\nEmail: {}\nPassword: {}\n\nLogin at: {}/login\n\nYou can now:\n- Set up your missed call responses\n- Configure call forwarding rules\n- Monitor your call activity\n\nFor help, contact support@missedcallrespondr.com\n\nBest regards,\nThe {} Team",
                app_name, name, email, password, app_url, app_name
            );
            send_email_request(
                pool,
                tenant_id,
                template_type,
                to,
                &format!("Welcome to {}!", app_name),
                &body,
                "",
            )
            .await
        }
        "purchase_confirmed" => {
            let body = format!(
                "Hi {},\n\nThank you for your purchase! Your payment for the {} plan has been received successfully.\n\nYou can access your dashboard at: {}/dashboard\n\nIf you have any questions, please contact support@missedcallrespondr.com\n\nBest regards,\nThe {} Team",
                name, plan_name_val, app_url, app_name
            );
            send_email_request(
                pool,
                tenant_id,
                template_type,
                to,
                "Payment Received - Thank You!",
                &body,
                "",
            )
            .await
        }
        "password_reset" => {
            let body = format!(
                "Your password reset code is: {}\n\nThis code expires in 1 hour.\n\nIf you did not request this password reset, please ignore this email.\n\n- SwiftSoftware",
                token
            );
            send_email_request(
                pool,
                tenant_id,
                template_type,
                to,
                "Password Reset Request",
                &body,
                "",
            )
            .await
        }
        _ => {
            let body = format!("{} Notification:\n\n{}", app_name, vars);
            send_email_request(
                pool,
                tenant_id,
                template_type,
                to,
                &format!("{} Notification", app_name),
                &body,
                "",
            )
            .await
        }
    }
}

/// Convenience wrapper — now uses DB template system
#[allow(dead_code)]
pub async fn send_welcome_email(
    pool: &PgPool,
    tenant_id: Uuid,
    to: &str,
    name: &str,
    password: &str,
) -> Result<(), String> {
    let vars = json!({
        "name": name,
        "email": to,
        "password": password,
        "app_url": "https://app.missedcallrespondr.com",
    });
    send_template_email(pool, tenant_id, to, "welcome", &vars).await
}

/// Convenience wrapper — now uses DB template system
#[allow(dead_code)]
pub async fn send_purchase_confirmed_email(
    pool: &PgPool,
    tenant_id: Uuid,
    to: &str,
    name: &str,
    plan_name: &str,
) -> Result<(), String> {
    let vars = json!({
        "name": name,
        "plan_name": plan_name,
        "app_url": "https://app.missedcallrespondr.com",
    });
    send_template_email(pool, tenant_id, to, "purchase_confirmed", &vars).await
}

/// Convenience wrapper — now uses DB template system
#[allow(dead_code)]
pub async fn send_reset_email(
    pool: &PgPool,
    tenant_id: Uuid,
    to: &str,
    token: &str,
) -> Result<(), String> {
    let vars = json!({
        "token": token,
        "name": "there",
        "app_url": "https://app.missedcallrespondr.com",
    });
    send_template_email(pool, tenant_id, to, "password_reset", &vars).await
}

/// Host of `api_url` for logging — the URL carries no secret, but the key never goes near a log.
fn url_host(api_url: &str) -> &str {
    api_url
        .split("://")
        .nth(1)
        .unwrap_or(api_url)
        .split('/')
        .next()
        .unwrap_or(api_url)
}

/// Record the outcome of the last send attempt where an operator can see it.
///
/// The defect this replaced was invisible from every surface: `auth::register` created the account,
/// the send failed, and only the container log said so. Every attempt (success AND failure) is now
/// recorded under `admin_settings.email_last_send` and served by
/// `GET /api/v1/admin/email-config`, so "the welcome mail never arrived" is answerable without
/// `docker logs`.
///
/// Best-effort by construction: a recording failure is logged and never changes the send's result.
async fn record_last_send(
    pool: &PgPool,
    kind: &str,
    to: &str,
    provider: &str,
    outcome: &Result<String, String>,
) {
    let payload = json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "kind": kind,
        "to": to,
        "provider": provider,
        "ok": outcome.is_ok(),
        "detail": match outcome {
            Ok(receipt) => receipt.clone(),
            Err(e) => e.clone(),
        },
    });
    if let Err(e) = sqlx::query(
        "INSERT INTO admin_settings (key, value, description, updated_at) \
         VALUES ($1, $2::jsonb, 'Last system-mail send attempt (written by the sender; admin-visible)', NOW()) \
         ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, updated_at = NOW()",
    )
    .bind(email_provider::LAST_SEND_KEY)
    .bind(payload.to_string())
    .execute(pool)
    .await
    {
        tracing::warn!("could not record the last mail-send outcome: {e}");
    }
}

/// Core email sender.
///
/// The provider, its credential and the BODY SHAPE are resolved by `email_provider` from the
/// panel-managed `admin_settings.email` row first and the `EMAIL_*` env as the fallback. Mailgun is
/// a form API: the previous implementation posted one JSON body with a top-level `"from"` to every
/// provider, so Mailgun authenticated the request and then answered
/// `400 {"message":"from parameter is missing"}` — the account was created and its welcome mail
/// could never be sent (kanban t_6d575da6). `kind` names what the message IS ("welcome",
/// "password_reset", …) so a failure is attributable on the admin surface.
async fn send_email_request(
    pool: &PgPool,
    tenant_id: Uuid,
    kind: &str,
    to: &str,
    subject: &str,
    text_body: &str,
    html_body: &str,
) -> Result<(), String> {
    // Fleet harness addresses never reach a real relay (parity with FunnelSwift, kanban t_36b55ed2).
    // A probe that signs up with a fleet-dev domain (`swiftsoftware.dev/.net`) is created normally but
    // its mail is withheld: the address is routable, so a send can only land in a fleet mailbox or
    // bounce (measured 2026-10-09 on mail.missedcallrespondr.com: `accepted` then `bounced` 552), and
    // every such send burns a delivery on the domain's sending reputation. This function is the choke
    // point BOTH the template arm and the inline fallbacks pass through, so the guard covers every
    // transactional type. The RFC-2606 class is now ALSO suppressed (changed 2026-10-09 after measuring real provider attempts) (.local/.test/.invalid/example.*) is deliberately NOT
    // suppressed — content harnesses point the provider at a local sink and read the message off the
    // wire, so silencing it would delete proof.
    if let Some(domain) = crate::security::probe_addr::harness_domain(to) {
        tracing::info!(
            to = %to,
            domain = %domain,
            kind = %kind,
            "send suppressed: recipient is a fleet harness address (fleet-dev domain)"
        );
        return Ok(());
    }
    // The account's branding opens both parts of the message (kanban t_feab8aff); a no-op for an
    // account with no branding, so those sends are byte-identical to before.
    let branding = branding::load(pool, tenant_id).await;
    let (text_body, html_body) = apply_branding(branding.as_ref(), text_body, html_body);

    let cfg = email_provider::resolve(pool).await.ok_or_else(|| {
        "no email provider configured — save the admin panel's Email Provider \
         (admin_settings.email) or set EMAIL_API_URL/EMAIL_API_KEY/EMAIL_FROM"
            .to_string()
    })?;

    let html = if html_body.is_empty() {
        None
    } else {
        Some(html_body.as_str())
    };
    let outcome = email_provider::deliver(&cfg, to, subject, &text_body, html).await;

    // The provider's own receipt on success (`{"id":"<…>","message":"Queued. Thank you."}`) is the
    // only proof the message left the box; on failure the status + body is what distinguishes a
    // rejected KEY from a rejected transport. Neither the key nor the URL's query is logged.
    match &outcome {
        Ok(receipt) => tracing::info!(
            "email.provider_response: provider={} source={} host={} kind={kind} to={to} receipt={receipt:?}",
            cfg.provider,
            cfg.source,
            url_host(&cfg.api_url)
        ),
        Err(e) => tracing::error!(
            "email.provider_error: provider={} source={} host={} kind={kind} to={to}: {e}",
            cfg.provider,
            cfg.source,
            url_host(&cfg.api_url)
        ),
    }

    record_last_send(pool, kind, to, &cfg.provider, &outcome).await;
    outcome.map(|_| ())
}

#[derive(Debug, sqlx::FromRow)]
pub struct EmailTemplateRow {
    #[allow(dead_code)]
    id: Uuid,
    #[allow(dead_code)]
    name: String,
    subject: Option<String>,
    body: Option<String>,
    html_body: Option<String>,
    is_html: Option<bool>,
    #[allow(dead_code)]
    is_default: Option<bool>,
}

#[cfg(test)]
mod render_tests {
    use super::{render_template, with_app_vars};
    use serde_json::json;

    /// The shape that was broken live: the shipped rows use a SINGLE brace, so a db template that
    /// was actually selected logged (and sent) `Welcome to {app_name}!` verbatim.
    #[test]
    fn single_brace_placeholders_render() {
        let vars = json!({"app_name": "MissedCall Respondr", "token": "abc123"});
        assert_eq!(
            render_template("Welcome to {app_name}!", &vars),
            "Welcome to MissedCall Respondr!"
        );
        assert_eq!(
            render_template("Your password reset code is: {token}", &vars),
            "Your password reset code is: abc123"
        );
    }

    /// The previously-documented convention must keep working — the old doc comment promised
    /// `{{key}}`, and the mcr-notype audit harness still writes it.
    #[test]
    fn double_brace_placeholders_still_render() {
        let vars = json!({"token": "abc123"});
        assert_eq!(render_template("code {{token}}", &vars), "code abc123");
        assert_eq!(render_template("code {token}", &vars), "code abc123");
    }

    /// The trap in the single-brace shape: substitution is KEY-DRIVEN, so a real brace that is not
    /// a placeholder is left byte-for-byte. A key that is absent from `vars` is also left literal —
    /// an unfilled placeholder must stay visible, never silently become "".
    #[test]
    fn literal_braces_that_are_not_placeholders_survive() {
        let vars = json!({"name": "David"});
        assert_eq!(render_template("{\"a\": 1}", &vars), "{\"a\": 1}");
        assert_eq!(
            render_template("body { color: red; }", &vars),
            "body { color: red; }"
        );
        assert_eq!(
            render_template("{not_a_key} and {name}", &vars),
            "{not_a_key} and David"
        );
    }

    /// The live welcome row, verbatim, against the vars `auth::register` supplies.
    #[test]
    fn live_welcome_row_renders_via_the_signup_vars() {
        let vars = with_app_vars(
            &json!({"name": "David", "email": "d@example.com", "password": "pw"}),
            "MissedCall Respondr",
            "https://app.missedcallrespondr.com",
        );
        assert_eq!(
            render_template("Welcome to {app_name}!", &vars),
            "Welcome to MissedCall Respondr!"
        );
        assert_eq!(
            render_template(
                "Login: {login_url}/login\nEmail: {email}\nPassword: {password}",
                &vars
            ),
            "Login: https://app.missedcallrespondr.com/login\nEmail: d@example.com\nPassword: pw"
        );
    }

    /// App-level defaults are filled in for every call site, and a caller that knows better wins.
    #[test]
    fn app_vars_are_defaults_caller_values_win() {
        let merged = with_app_vars(
            &json!({"app_name": "Caller Wins"}),
            "MissedCall Respondr",
            "https://app.missedcallrespondr.com",
        );
        assert_eq!(merged["app_name"], json!("Caller Wins"));
        assert_eq!(
            merged["login_url"],
            json!("https://app.missedcallrespondr.com")
        );
        assert_eq!(
            merged["app_url"],
            json!("https://app.missedcallrespondr.com")
        );
    }
}
