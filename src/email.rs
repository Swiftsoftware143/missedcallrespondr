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

/// The app's OWN wordmark, used for the header of every message sent for an account that has
/// configured no branding of its own (kanban t_6a8af451).
///
/// A mail client fetches it over the network with no session, so it is an absolute URL on a host
/// that answers publicly — and it is a PNG, because Gmail and Outlook do not render the SVG the
/// pages use. `alt` carries the name, so a client that blocks images still says whose mail this is.
const APP_LOGO_PATH: &str = "/logo.png";

/// The identity every message falls back to when its account has configured none: the app's own
/// name and wordmark, in the fleet's default accent.
///
/// Before this the header block only appeared once an account had uploaded a logo of its own, so
/// the credentials mail a BRAND-NEW customer receives — the first thing this product ever shows
/// them — arrived with no wordmark at all, while the branding preview told the operator it
/// "carries the MissedCall Respondr header" (measured live 2026-10-24: no `<img>`, no header div,
/// no colour rule and no HTML part at all on the captured message). An account's own branding
/// still wins field by field.
fn app_default_branding() -> branding::Branding {
    branding::Branding {
        brand_name: branding::APP_NAME.to_string(),
        brand_color: String::new(),
        logo_url: APP_LOGO_PATH.to_string(),
    }
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
        None => (
            branding::APP_NAME.to_string(),
            app_default_branding()
                .resolve_logo_url(branding::APP_URL)
                .unwrap_or_default(),
        ),
    };
    // Whether the logo above is the account's OWN upload or this app's wordmark standing in for
    // one. `logo_url` is always non-empty now, so copy that describes the header reads this flag
    // instead of testing the URL — and never calls the app's own header the tenant's.
    let logo_is_own = branding
        .and_then(|b| b.resolve_logo_url(branding::APP_URL))
        .is_some();
    if let Some(obj) = vars.as_object_mut() {
        obj.insert("brand_name".to_string(), json!(name));
        obj.insert("logo_url".to_string(), json!(logo));
        obj.insert(
            "logo_is_own".to_string(),
            json!(if logo_is_own { "yes" } else { "no" }),
        );
    }
}

/// Put the account's branding at the TOP of a rendered message (kanban t_feab8aff).
///
/// The HTML part gains the header block (logo + name + colour rule); the text part gains the brand
/// name as a one-line header (a text part cannot carry an image). When the account HAS branding but
/// the message carries no HTML part — the inline fallbacks return text only — one is built from the
/// ESCAPED text so a branded mail still shows the logo. `None` stays a no-op — that is the shape
/// the additivity test pins — while every SEND passes `Some`, because the send path substitutes the
/// app's own identity for an account that has none (kanban t_6a8af451).
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
    //
    // The MERGE fields follow the STORED document (an account with a name but no logo keeps its own
    // name and reports `logo_is_own = no`), while the header below follows the account's branding
    // or, when it has none, this app's own identity — an unbranded account's mail must still say
    // whose it is (kanban t_6a8af451).
    let stored_branding = branding::load(pool, tenant_id).await;
    bind_branding(&mut vars, stored_branding.as_ref());

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
        // The sign-in address change (kanban t_0d1aa081). Named for what the recipient must DO,
        // because the account has not moved yet — the link is the only thing that moves it.
        "email_change" => "Confirm your new sign-in email".to_string(),
        // The operator answered a ticket (inbox ticket-reply notice). Named for what the customer
        // asked about, because "MissedCall Respondr Notification" tells them nothing and reads like
        // marketing on a thread they started.
        "ticket_reply" => "We answered your request".to_string(),
        // The receipt for a filed plan request (cron 2026-10-20). Named for what the customer just
        // did, so it is not mistaken for the answer itself — the answer arrives later, from a person.
        "billing_request_receipt" => format!("We have your plan request — {}", app_name),
        // The operator's own branding sample (cron 2026-10-24, parity with FunnelSwift,
        // IncentiveSwift and ADASwift). Named for what it IS, so it cannot be mistaken for the
        // missed-call reply a caller receives.
        "branding_preview" => format!("A preview of your {} email branding", app_name),
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
                "Welcome to {}, {}!\n\nYour account has been created successfully.\n\nHere are your login credentials:\n\nEmail: {}\nPassword: {}\n\nLogin at: {}/login\n\nYou can now:\n- Set up your missed call responses\n- Configure call forwarding rules\n- Monitor your call activity\n\nFor help, contact support@swiftsoftware.net\n\nBest regards,\nThe {} Team",
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
                "Hi {},\n\nThank you for your purchase! Your payment for the {} plan has been received successfully.\n\nYou can access your dashboard at: {}/dashboard\n\nIf you have any questions, please contact support@swiftsoftware.net\n\nBest regards,\nThe {} Team",
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
            // The customer must be TOLD where to enter the code, and the mail must carry THIS app's
            // name. It used to hand over a 36-character code with no destination AND sign itself
            // "SwiftSoftware" — a company the customer has never heard of, on a mail that otherwise
            // belongs to MissedCall Respondr (fixed 2026-10-13, cron `default`).
            // CORRECTED 2026-10-16 (cron `default`): the destination it was given was `/login`, and
            // that page has NO code field — its "Forgot password?" link goes to /forgot-password.html,
            // which asks for the ADDRESS and mails ANOTHER code. The page that accepts the code is
            // /reset-password.html (input id="t", prefilled from `?token=`), so that is what the mail
            // links to now, and the expiry it states is the 24h `auth::handlers::forgot_password`
            // actually mints.
            let body = format!(
                "Your password reset code is: {}\n\nOpen {}/reset-password.html?token={} to choose a new password - the code is filled in for you. If the form does not open, go to {}/reset-password.html and paste the code above.\n\nThis code expires in 24 hours.\n\nIf you did not request this password reset, please ignore this email.\n\n- The {} Team",
                token, app_url, token, app_url, app_name
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
        // The inline safety net for the sign-in address change (kanban t_0d1aa081) — reached when
        // the account has no `email_change` template row. Two things the customer must be told, and
        // both were learned from the reset mail's own history: WHERE to click (a link, not a code),
        // and that NOTHING HAS CHANGED YET (the old address stays valid until the link is opened).
        "email_change" => {
            let new_email = vars
                .get("new_email")
                .and_then(|v| v.as_str())
                .unwrap_or("(the new address)");
            let link = vars.get("link").and_then(|v| v.as_str()).unwrap_or(app_url);
            let body = format!(
                "Hi {},\n\nWe received a request to change the email address you sign in to {} with, to:\n\n    {}\n\nTo confirm, open this link:\n\n    {}\n\nThe link works once and expires in 2 hours. Until you open it, nothing changes: your current address still works and your password stays the same. If you did not ask for this, ignore this email and nothing will happen.\n\n- The {} Team",
                name, app_name, new_email, link, app_name
            );
            send_email_request(
                pool,
                tenant_id,
                template_type,
                to,
                "Confirm your new sign-in email",
                &body,
                "",
            )
            .await
        }
        // The reply notice. Reached whenever the tenant has no `ticket_reply` template row (or the
        // query failed), which is the normal state until an operator edits one — so this body is the
        // REAL mail most customers get, not a placeholder: it repeats the operator's own words.
        "ticket_reply" => {
            let subject_line = vars
                .get("ticket_subject")
                .and_then(|v| v.as_str())
                .unwrap_or("your request");
            let reply = vars
                .get("reply_body")
                .and_then(|v| v.as_str())
                .unwrap_or("(no message body)");
            let body = format!(
                "Hi {name},\n\nWe answered your request \"{subject_line}\".\n\n{reply}\n\nOpen {app_url}/dashboard and go to Tickets to reply or see the whole thread.\n\n- The {app_name} Team"
            );
            send_email_request(
                pool,
                tenant_id,
                template_type,
                to,
                &format!("We answered your request: {subject_line}"),
                &body,
                "",
            )
            .await
        }
        // The receipt for a plan request (cron 2026-10-20). Reached whenever the tenant has no
        // `billing_request_receipt` template row, which is the normal state on a fresh install — so
        // this body is the REAL mail most customers get, not a placeholder. On an install with no
        // payment provider the Plan & Billing screen's request IS the buying path, and until this
        // arm existed the row was written while the customer's inbox stayed empty.
        "billing_request_receipt" => {
            let request_subject = vars
                .get("request_subject")
                .and_then(|v| v.as_str())
                .unwrap_or("your request");
            let body = format!(
                "Hi {name},\n\nThanks — we have your request:\n\n    {request_subject}\n\nNothing has been charged. A person (not a robot) reads it and answers you by email, and you can see where it stands under Tickets on your dashboard: {app_url}/dashboard\n\nIf you would rather not go ahead, reply to this message and we will close it.\n\n- The {app_name} Team"
            );
            send_email_request(
                pool,
                tenant_id,
                template_type,
                to,
                &format!("We have your plan request — {app_name}"),
                &body,
                "",
            )
            .await
        }
        // The operator's own branding PREVIEW (cron 2026-10-24, parity with FunnelSwift,
        // IncentiveSwift and ADASwift). The header the operator is judging is added by
        // `apply_branding` at the send choke point — that is the account's real logo, name and
        // colour on a real message — so the body itself stays deliberately plain and says what the
        // reader is looking at. Text-only on purpose: every other mail this app sends is text-only
        // and gets the same branded header, so the sample is the mail a caller really gets.
        "branding_preview" => {
            // The header is ALWAYS there now (an unbranded account gets this app's own wordmark),
            // so the copy reads the `logo_is_own` merge field instead of assuming the logo above is
            // the operator's: an upload they never made must not be described as theirs, and this
            // app's own header must not be called "YOUR branding" (kanban t_6a8af451).
            let header_line = if vars.get("logo_is_own").and_then(|v| v.as_str()) == Some("yes") {
                "The header above this line is YOUR branding: your logo, your display name and your colour. If it is right here, it is right on every missed-call reply, invite and password email your callers receive."
            } else {
                "No logo of your own is set yet, so this message carries the MissedCall Respondr wordmark and name. Upload a logo under Email Branding on your dashboard, save it, and send yourself another preview to see your own."
            };
            let body = format!(
                "Hi {name},\n\nThis is a sample of the mail {app_name} sends on your behalf.\n\n{header_line}\n\nChange it any time under Email Branding on your dashboard: {app_url}/dashboard\n\n- The {app_name} Team"
            );
            send_email_request(
                pool,
                tenant_id,
                template_type,
                to,
                &format!("A preview of your {app_name} email branding"),
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

/// The operator's own in-inbox proof that their branding is applied (cron 2026-10-24, parity with
/// FunnelSwift, IncentiveSwift and ADASwift). The recipient is ALWAYS the caller's own address —
/// this is a preview, never a relay — and a reserved/harness address never reaches a relay at all:
/// `send_email_request` reports it as a non-send instead of a false success.
pub async fn send_branding_preview_email(
    pool: &PgPool,
    tenant_id: Uuid,
    to: &str,
    name: &str,
) -> Result<(), String> {
    let vars = json!({
        "name": name,
        "email": to,
        "app_url": "https://app.missedcallrespondr.com",
    });
    send_template_email(pool, tenant_id, to, "branding_preview", &vars).await
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
pub async fn record_last_send(
    pool: &PgPool,
    kind: &str,
    to: &str,
    provider: &str,
    outcome: &Result<String, String>,
) {
    // `deliver` answers Ok("suppressed: …") for a reserved/harness recipient — it deliberately
    // attempts nothing — so Ok alone is not a delivery. Recording that as accepted is the misreport
    // this line exists to prevent (measured 2026-10-14 on the admin test route), so the withheld
    // receipt is recorded as the non-send it is.
    let suppressed = matches!(outcome, Ok(receipt) if receipt.starts_with("suppressed"));
    let detail = if suppressed {
        format!(
            "withheld: '{to}' is a reserved test address, so nothing was sent (no provider request)"
        )
    } else {
        match outcome {
            Ok(receipt) => receipt.clone(),
            Err(e) => e.clone(),
        }
    };
    // `withheld` is a status of its own, not a failure: nothing was attempted. The admin test route
    // hands this function `Err("withheld: …")` rather than the transport's `suppressed` receipt, so
    // the detail prefix is the second signal — both call sites then classify identically.
    let status = if suppressed || detail.starts_with("withheld:") {
        "withheld"
    } else if outcome.is_ok() {
        "sent"
    } else {
        "failed"
    };
    let payload = json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "kind": kind,
        "to": to,
        "provider": provider,
        "ok": status == "sent",
        "detail": detail,
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

    // Append the SAME outcome to the HISTORY (migration 000040). The row above is overwritten by the
    // next send, so without this a credentials mail the provider refused at signup vanished behind
    // any later success and "did this customer ever get their password?" had no answer. Best-effort
    // like the write above: a bookkeeping failure must never change the send's own result.
    if let Err(e) = sqlx::query(
        "INSERT INTO email_send_log (to_email, template, provider, status, detail) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(to)
    .bind(kind)
    .bind(provider)
    .bind(status)
    .bind(&detail)
    .execute(pool)
    .await
    {
        tracing::warn!("could not append the mail-send history (email_send_log): {e}");
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
        // Record the withheld outcome on the SAME panel-visible line a real send writes. Returning
        // silently left the previous, unrelated "accepted" line standing, so an operator reading
        // "the last mail was accepted" had no way to know this one was never sent.
        record_last_send(
            pool,
            kind,
            to,
            "suppressed",
            &Err(format!(
                "withheld: '{domain}' is a fleet-harness / reserved address, so nothing was sent"
            )),
        )
        .await;
        return Ok(());
    }
    // The account's branding opens both parts of the message (kanban t_feab8aff). An account with
    // NO branding of its own gets this app's own wordmark and name instead of an empty space
    // (kanban t_6a8af451): the first mail a brand-new customer receives is the credentials mail, and
    // it must say who it is from. `apply_branding` still takes an Option so its additivity test
    // keeps pinning "None changes nothing"; every SEND resolves to `Some` here.
    let branding = branding::load(pool, tenant_id)
        .await
        .unwrap_or_else(app_default_branding);
    let (text_body, html_body) = apply_branding(Some(&branding), text_body, html_body);

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

#[cfg(test)]
mod branding_tests {
    use super::{app_default_branding, apply_branding, bind_branding, APP_LOGO_PATH};
    use crate::branding;
    use serde_json::json;

    /// The defect this card fixes (kanban t_6a8af451): an account with no branding of its own used
    /// to send mail with NO header at all, so the very first message a new customer receives — the
    /// credentials mail — carried no wordmark, while the branding preview claimed it "carries the
    /// MissedCall Respondr header".
    #[test]
    fn an_unbranded_account_gets_the_apps_own_wordmark() {
        let b = app_default_branding();
        assert_eq!(b.brand_name, branding::APP_NAME);
        assert_eq!(b.logo_url, APP_LOGO_PATH);
        let url = b
            .resolve_logo_url(branding::APP_URL)
            .expect("the app wordmark resolves");
        // A mail client resolves nothing relative: the PNG must be absolute, on the app's own host.
        assert_eq!(url, "https://app.missedcallrespondr.com/logo.png");
        let header = b.header_html(Some(&url));
        assert!(header.contains("<img"), "{header}");
        assert!(header.contains("alt=\"MissedCall Respondr\""), "{header}");
        assert!(
            header.contains(branding::DEFAULT_BRAND_COLOR),
            "the app's own header is painted in the fleet accent: {header}"
        );
    }

    /// The header must reach a TEXT-ONLY message too: every transactional mail in this app passes
    /// `html = ""`, so `apply_branding` builds a part for the header to live in.
    #[test]
    fn a_text_only_send_still_carries_the_header() {
        let b = app_default_branding();
        let (text, html) = apply_branding(Some(&b), "Hi there", "");
        assert!(text.starts_with("MissedCall Respondr\n\n"), "{text}");
        assert!(html.contains("<img"), "{html}");
        assert!(html.contains("Hi there"), "{html}");
    }

    /// `None` stays a no-op — the additivity shape the port pinned. Every SEND resolves to `Some`
    /// (the app's identity), so this arm is only reachable from a test now.
    #[test]
    fn none_still_changes_nothing() {
        let (text, html) = apply_branding(None, "Hi there", "<p>Hi there</p>");
        assert_eq!(text, "Hi there");
        assert_eq!(html, "<p>Hi there</p>");
    }

    /// `logo_url` is never empty on the wire now, so copy that describes the header reads the flag.
    #[test]
    fn the_merge_fields_say_whose_logo_the_reader_is_looking_at() {
        let mut unbranded = json!({"name": "Ada"});
        bind_branding(&mut unbranded, None);
        assert_eq!(unbranded["brand_name"], json!(branding::APP_NAME));
        assert_eq!(
            unbranded["logo_url"],
            json!("https://app.missedcallrespondr.com/logo.png")
        );
        assert_eq!(unbranded["logo_is_own"], json!("no"));

        // An account's own upload: its own URL, and the flag tells the copy it is theirs.
        let own = branding::Branding {
            brand_name: "Acme Plumbing".into(),
            brand_color: "#0ea5e9".into(),
            logo_url: "/api/v1/branding/logo/abc?v=7".into(),
        };
        let mut branded = json!({"name": "Ada"});
        bind_branding(&mut branded, Some(&own));
        assert_eq!(branded["brand_name"], json!("Acme Plumbing"));
        assert_eq!(
            branded["logo_url"],
            json!("https://app.missedcallrespondr.com/api/v1/branding/logo/abc?v=7")
        );
        assert_eq!(branded["logo_is_own"], json!("yes"));

        // A name with no logo keeps the account's name and reports `no` — the preview must not
        // promise a logo that is not in the header.
        let name_only = branding::Branding {
            brand_name: "Acme Plumbing".into(),
            brand_color: String::new(),
            logo_url: String::new(),
        };
        let mut mine = json!({});
        bind_branding(&mut mine, Some(&name_only));
        assert_eq!(mine["brand_name"], json!("Acme Plumbing"));
        assert_eq!(mine["logo_url"], json!(""));
        assert_eq!(mine["logo_is_own"], json!("no"));
    }
}
