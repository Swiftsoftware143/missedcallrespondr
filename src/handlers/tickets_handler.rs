use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    models::ticket::{
        CreateTicketMessageRequest, CreateTicketRequest, Ticket, TicketMessage, UpdateTicketRequest,
    },
    state::AppState,
    validation::{check_opt_len, max},
};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub status: Option<String>,
    pub priority: Option<String>,
    /// EXACT source filter (`plan_request`, `billing`, `manual`, …). Added for the operator's Plan
    /// Requests queue, which asks for the two sources a plan change is filed under. Omitted, every
    /// source comes back, so no existing caller changes shape.
    pub source: Option<String>,
}

/// Is the caller a PLATFORM OPERATOR — the team that answers plan requests?
///
/// The customer consoles file a plan change as a `source = "plan_request"` ticket (the Plan & Billing
/// screen's "Request the <plan> plan" button; missedcall filed `billing` until 2026-10-20, so the
/// operator's figures count BOTH — see `admin_handler::platform_stats`). Until this predicate existed every read in this module
/// was scoped to `claims.aid`, so a request filed by a customer lived in a workspace the operator
/// could not open: measured live 2026-10-20, the customer's POST answered 200 (and mailed them a
/// receipt) while the operator's `GET /api/v1/tickets` returned **0 rows** and
/// `GET`/`PUT /api/v1/tickets/:id` answered **404** for the very id the customer had just been given.
/// The fleet's other apps close the same gap from the same direction (IncentiveSwift reads the whole
/// queue for an operator; WorkflowSwift has an admin Plan Requests screen).
///
/// This only ever WIDENS what a platform role can see. A customer keeps the strict workspace scope,
/// so no ticket becomes readable by another tenant's customer.
fn caller_is_operator(claims: &Claims) -> bool {
    matches!(
        claims.role.as_str(),
        "admin" | "super_admin" | "agency_admin"
    )
}

/// A ticket as the OPERATOR's queue shows it: the row plus whose workspace filed it.
///
/// `tenant_name` and `account_email` are what make a request actionable — "Upgrade request: Starter
/// plan" alone does not say which customer asked. They are joined in the query rather than fetched per
/// row, and `COALESCE` keeps the shape of the JSON fixed when a tenant or owner is missing.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct OperatorTicket {
    pub id: Uuid,
    /// `None` when the message came from the public contact page — the visitor has no workspace.
    pub tenant_id: Option<Uuid>,
    pub subject: String,
    pub status: String,
    pub priority: String,
    pub assigned_to: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub source: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub tenant_name: String,
    pub account_email: String,
    /// The address a visitor typed on the contact page (2026-11-06). Empty for a ticket filed from
    /// inside a workspace, whose owner is already named by `account_email` — so the queue row always
    /// says WHO to answer.
    pub requester_email: String,
    /// The tier the request asks for (cron 2026-10-20), and its catalogue name — so the operator row
    /// says "Pro", not a slug, and a request for an ordinary ticket stays empty.
    pub plan_slug: Option<String>,
    pub plan_name: Option<String>,
    /// When the resolve seated it; `None` = still waiting.
    pub plan_applied_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// The ticket `id`, as this caller is allowed to reach it.
///
/// One place decides the access rule so `get`, `update`, `add_message` and `delete` cannot drift
/// apart: a customer sees their own workspace's ticket, an operator sees any ticket. A row the caller
/// may not reach is a 404 (not a 403) — a 403 would confirm that someone else's ticket exists.
async fn ticket_visible_to(
    pool: &sqlx::PgPool,
    claims: &Claims,
    id: Uuid,
) -> Result<Ticket, AppError> {
    let row = if caller_is_operator(claims) {
        sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await?
    } else {
        sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(pool)
            .await?
    };
    row.ok_or_else(|| AppError::NotFound("Ticket not found".into()))
}

pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    // An OPERATOR reads the WHOLE queue (cron 2026-10-20). A customer's plan request is a ticket in
    // THEIR workspace, and scoping the read to the operator's own workspace made the one screen built
    // to work those requests permanently empty. The customer arm below keeps the strict scope.
    if caller_is_operator(&claims) {
        let rows = sqlx::query_as::<_, OperatorTicket>(
            r#"SELECT t.id, t.tenant_id, t.subject, t.status, t.priority, t.assigned_to,
                      t.contact_id, t.source, t.created_at, t.updated_at,
                      COALESCE(tn.name, CASE WHEN t.tenant_id IS NULL THEN 'Website visitor' ELSE '' END) AS tenant_name,
                      COALESCE(u.email, '') AS account_email,
                      COALESCE(t.requester_email, '') AS requester_email,
                      t.plan_slug,
                      COALESCE(p.name, t.plan_slug) AS plan_name,
                      t.plan_applied_at
               FROM tickets t
               LEFT JOIN plans p ON p.slug = t.plan_slug
               LEFT JOIN tenants tn ON tn.id = t.tenant_id
               LEFT JOIN LATERAL (
                   SELECT email FROM users
                   WHERE tenant_id = t.tenant_id
                   ORDER BY (role IN ('admin','account_owner')) DESC, created_at ASC
                   LIMIT 1
               ) u ON TRUE
               WHERE ($1::text IS NULL OR t.source = $1)
               ORDER BY t.created_at DESC
               LIMIT $2 OFFSET $3"#,
        )
        .bind(&q.source)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?;
        return Ok(Json(serde_json::json!(rows)));
    }
    let items = match (&q.status, &q.priority) {
        (Some(s), Some(p)) => sqlx::query_as::<_, Ticket>(
            "SELECT * FROM tickets WHERE tenant_id = $1 AND status = $2 AND priority = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5",
        )
        .bind(claims.aid).bind(s).bind(p).bind(limit).bind(offset).fetch_all(&state.pool).await?,
        (Some(s), None) => sqlx::query_as::<_, Ticket>(
            "SELECT * FROM tickets WHERE tenant_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid).bind(s).bind(limit).bind(offset).fetch_all(&state.pool).await?,
        (None, Some(p)) => sqlx::query_as::<_, Ticket>(
            "SELECT * FROM tickets WHERE tenant_id = $1 AND priority = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4",
        )
        .bind(claims.aid).bind(p).bind(limit).bind(offset).fetch_all(&state.pool).await?,
        (None, None) => sqlx::query_as::<_, Ticket>(
            "SELECT * FROM tickets WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
        )
        .bind(claims.aid).bind(limit).bind(offset).fetch_all(&state.pool).await?,
    };
    Ok(Json(serde_json::json!(items)))
}

pub async fn stats(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    // The counters follow the same rule as the queue: an operator's numbers are the fleet's ticket
    // numbers, a customer's are their own. A queue that lists another workspace's request while the
    // counters say zero is the kind of half-truth that makes an operator distrust both.
    let row = if caller_is_operator(&claims) {
        sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
            "SELECT \
                COUNT(*) FILTER (WHERE status='open'), \
                COUNT(*) FILTER (WHERE status='in_progress'), \
                COUNT(*) FILTER (WHERE status='resolved'), \
                COUNT(*) FILTER (WHERE status='closed'), \
                COUNT(*) \
             FROM tickets",
        )
        .fetch_one(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, (i64, i64, i64, i64, i64)>(
            "SELECT \
                COUNT(*) FILTER (WHERE status='open'), \
                COUNT(*) FILTER (WHERE status='in_progress'), \
                COUNT(*) FILTER (WHERE status='resolved'), \
                COUNT(*) FILTER (WHERE status='closed'), \
                COUNT(*) \
             FROM tickets WHERE tenant_id = $1",
        )
        .bind(claims.aid)
        .fetch_one(&state.pool)
        .await?
    };
    Ok(Json(serde_json::json!({
        "open": row.0, "in_progress": row.1, "resolved": row.2, "closed": row.3, "total": row.4
    })))
}

/// What the public contact page posts (`POST /api/v1/public/support-request`).
#[derive(Deserialize)]
pub struct PublicTicketBody {
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
    pub subject: String,
    /// The visitor's words. The served page sends `body`; `message` is accepted as an alias so a
    /// hand-written client is not refused for naming the same field differently.
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// What the contact page posts to read the team's ANSWER back (`POST /api/v1/public/support-answer`).
#[derive(Deserialize)]
pub struct PublicAnswerBody {
    pub ticket_id: String,
    pub email: String,
}

/// How many messages ONE address may file through the public contact page per hour.
///
/// The arm is anonymous, so without a bound one script could fill the operator's queue. Three is
/// enough for a person writing about a lost password, then a correction.
const PUBLIC_HOURLY_LIMIT: i64 = 3;

/// Trim and cap a free-text field by CHARACTERS (not bytes): the cap is the client's to exceed and
/// must never split a multi-byte character.
fn cap_chars(raw: &str, max: usize) -> String {
    raw.trim()
        .chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The public contact page's filing arm (2026-11-06).
///
/// MEASURED (dns.google and cloudflare-dns.com agree): MX = 0 for this app's domain and every
/// sibling's, and nothing on this host listens on port 25, so every "write to support@…" sentence
/// this app printed sent a customer's message into a bounce — and a locked-out customer, the person
/// who needs help most, had no way to reach anyone.
///
/// This arm files the message into the operator's EXISTING ticket queue: one ticket with no workspace
/// (`tenant_id` NULL, migration 000048), the address the visitor typed in `requester_email`, and the
/// message as the thread's opening entry. The operator answers it from the queue they already work,
/// and `add_message` mails that answer to this address (template `support_reply`).
///
/// Guards, all refusing BEFORE a row is written: a syntactically real address, a non-empty bounded
/// message, at most three messages from one address per hour (429), and idempotence on an already
/// open ticket with the same address and subject.
pub async fn create_public(
    State(state): State<AppState>,
    Json(req): Json<PublicTicketBody>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let email = crate::security::email_addr::normalize(&req.email).map_err(|e| {
        AppError::Unprocessable(format!("{e} — we need an address we can reply to"))
    })?;
    let name = cap_chars(req.name.as_deref().unwrap_or(""), 120);
    let message = cap_chars(
        req.body.as_deref().or(req.message.as_deref()).unwrap_or(""),
        4000,
    );
    if message.is_empty() {
        return Err(AppError::Unprocessable(
            "Write your message so the team knows what you need.".into(),
        ));
    }
    let subject = cap_chars(&req.subject, 200);
    if subject.is_empty() {
        return Err(AppError::Unprocessable(
            "Give your message a short subject so the team knows what it is about.".into(),
        ));
    }
    let description = if name.is_empty() {
        message
    } else {
        format!("{name} wrote:\n\n{message}")
    };

    let recent: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tickets WHERE lower(requester_email) = $1 \
           AND created_at > now() - interval '1 hour'",
    )
    .bind(&email)
    .fetch_one(&state.pool)
    .await?;
    if recent >= PUBLIC_HOURLY_LIMIT {
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "status": 429,
                "error": "You have sent us several messages in the last hour. Please wait a little, or reply to the email we already have from you.",
                "retry_after_seconds": 3600
            })),
        ));
    }

    // A second identical message is not a second request: the form stays pressable after a send, so a
    // visitor who pressed twice must not mint a twin the operator has to work twice.
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tickets WHERE tenant_id IS NULL AND lower(requester_email) = $1 \
           AND lower(subject) = lower($2) AND status IN ('open','in_progress') \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&email)
    .bind(&subject)
    .fetch_optional(&state.pool)
    .await?;
    if let Some(id) = existing {
        return Ok((
            StatusCode::OK,
            Json(serde_json::json!({
                "id": id,
                "status": "existing",
                "message": "We already have your message with that subject — the team will reply to the address you gave."
            })),
        ));
    }

    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO tickets (id, tenant_id, subject, status, priority, assigned_to, contact_id, source, created_at, updated_at, requester_email) \
         VALUES ($1, NULL, $2, 'open', 'medium', NULL, NULL, 'contact_form', $3, $3, $4)",
    )
    .bind(id)
    .bind(&subject)
    .bind(now)
    .bind(&email)
    .execute(&state.pool)
    .await?;
    // The visitor's words are the thread's opening entry, labelled as theirs so the queue never reads
    // as if the team had said it.
    sqlx::query(
        "INSERT INTO ticket_messages (id, ticket_id, sender_type, sender_id, body, created_at) VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind("customer")
    .bind(None::<Uuid>)
    .bind(&description)
    .bind(now)
    .execute(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": id,
            "status": "open",
            "message": "Your message is with our team. We will reply to the address you gave."
        })),
    ))
}

/// `POST /api/v1/public/support-answer` — read the team's answer to a message filed from the
/// contact page (2026-11-06).
///
/// WHY THIS EXISTS. A visitor who filed from `contact.html` is not signed in and has no workspace,
/// so there is no Tickets screen they could ever load: the answer an operator writes lands in
/// `ticket_messages` on a row nobody who asked can open. The reply is also mailed, and mail is one
/// channel — a spam folder, a mistyped address, a customer who never checks it — so the answer
/// needs a second door. ADASwift, WorkflowSwift, FunnelSwift and IncentiveSwift already ship this
/// read-back; this app mailed and could not show.
///
/// The reference is an unguessable uuid and the address is the one they filed with, so the PAIR is
/// the credential. Either one alone answers the same neutral 404, which keeps the route from
/// confirming that a reference exists. Narrow by construction: ONE row, only when it carries no
/// workspace (a workspace ticket is read by its own signed-in users), returning the subject, the
/// status and the TEAM's entries — never the description, never a list of tickets, never another
/// visitor's row.
pub async fn public_answer(
    State(state): State<AppState>,
    Json(req): Json<PublicAnswerBody>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Neutral by design: an unknown reference, a malformed one and a reference that belongs to a
    // different address all answer the same sentence, so the route cannot be used to test whether
    // a reference is real.
    let missing = || {
        AppError::NotFound(
            "We have no message from that address with that reference. Check the reference and the \
             email address you filed with."
                .to_string(),
        )
    };
    let id = Uuid::parse_str(req.ticket_id.trim()).map_err(|_| missing())?;
    // The filing arm stores the address normalised, so the lookup normalises the same way —
    // `Probe@Example.com ` must find the row filed as `probe@example.com`.
    let email = crate::security::email_addr::normalize(&req.email).map_err(|_| missing())?;

    let some: Option<(String, String)> = sqlx::query_as(
        "SELECT subject, status FROM tickets \
          WHERE id = $1 AND tenant_id IS NULL AND lower(requester_email) = $2",
    )
    .bind(id)
    .bind(&email)
    .fetch_optional(&state.pool)
    .await?;
    let Some((subject, status)) = some else {
        return Err(missing());
    };

    // The TEAM's entries only. The filing arm labels the visitor's own words `customer`; this app's
    // own code treats `customer | user | client | business_user` as the customer side (see
    // `add_message`'s notification test), so excluding exactly those four returns what an operator
    // wrote and nothing a customer typed.
    let entries: Vec<(String, String)> = sqlx::query_as(
        "SELECT body, created_at::text FROM ticket_messages \
          WHERE ticket_id = $1 \
            AND COALESCE(sender_type, '') NOT IN ('customer', 'user', 'client', 'business_user') \
          ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(serde_json::json!({
        "id": id.to_string(),
        "subject": subject,
        "status": status,
        // An empty list means "not answered yet", which the page says in its own words rather than
        // showing an empty green box.
        "replies": entries
            .iter()
            .map(|(b, t)| serde_json::json!({ "body": b, "sent_at": t }))
            .collect::<Vec<_>>(),
    })))
}

pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateTicketRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    check_opt_len("priority", req.priority.as_deref(), max::TICKETS_PRIORITY)?;
    check_opt_len("source", req.source.as_deref(), max::TICKETS_SOURCE)?;
    check_opt_len(
        "sender_type",
        req.sender_type.as_deref(),
        max::TICKET_MESSAGES_SENDER_TYPE,
    )?;
    crate::features::check_feature_limit(&state.pool, claims.aid, "max_tickets", "Tickets").await?;
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    // The tier a plan request ASKS FOR travels with the row (cron 2026-10-20). It used to live only as
    // words inside `subject` ("Upgrade request: Pro plan"), so the operator's resolve had nothing
    // machine-readable to seat and the customer stayed on the free plan while being told the request
    // was handled — measured live. Validated against the ACTIVE catalogue here, so a typo or a retired
    // tier is a 400 at filing time instead of a request no operator can match to a plan.
    let plan_slug = req
        .plan_slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    if let Some(slug) = plan_slug.as_deref() {
        let known: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM plans WHERE slug = $1 AND is_active = true")
                .bind(slug)
                .fetch_optional(&state.pool)
                .await?;
        if known.is_none() {
            return Err(AppError::BadRequest(format!(
                "No active plan with slug '{slug}'"
            )));
        }
    }
    // A SECOND identical request is not a second request (measured live 2026-10-21). The Plan &
    // Billing button stays pressable after a request is filed, so a customer who pressed it twice
    // filed two OPEN tickets for the same plan and was mailed a receipt for each: duplicate work for
    // the operator, two mails to one inbox, and a request list that reads as if the first press had
    // been ignored. Their intent is unchanged, so the EXISTING open request is returned (flagged
    // `already_open`) instead of minting a twin — and no second receipt is queued, because the first
    // one already told them truthfully that the request had been received.
    //
    // The key is the TIER ASKED FOR (`plan_slug`, the machine-readable column added 2026-10-20), so a
    // customer with a Pro request open can still file a different plan's request. A legacy row with
    // no plan_slug falls back to the subject it was filed under.
    if matches!(
        req.source.as_deref().unwrap_or("manual"),
        "billing" | "plan_request"
    ) {
        let existing = sqlx::query_as::<_, Ticket>(
            "SELECT * FROM tickets \
             WHERE tenant_id = $1 \
               AND source IN ('billing','plan_request') \
               AND status IN ('open','in_progress') \
               AND ((plan_slug IS NOT NULL AND plan_slug = $2) \
                    OR (plan_slug IS NULL AND subject = $3)) \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(claims.aid)
        .bind(plan_slug.as_deref())
        .bind(&req.subject)
        .fetch_optional(&state.pool)
        .await?;
        if let Some(ticket) = existing {
            let mut out = serde_json::to_value(&ticket).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(obj) = out.as_object_mut() {
                obj.insert("acknowledged".to_string(), serde_json::json!(false));
                obj.insert("already_open".to_string(), serde_json::json!(true));
            }
            return Ok(Json(out));
        }
    }
    sqlx::query(
        "INSERT INTO tickets (id, tenant_id, subject, status, priority, assigned_to, contact_id, source, created_at, updated_at, plan_slug) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(id)
    .bind(claims.aid)
    .bind(&req.subject)
    .bind("open")
    .bind(req.priority.as_deref().unwrap_or("medium"))
    .bind(req.assigned_to)
    .bind(req.contact_id)
    .bind(req.source.as_deref().unwrap_or("manual"))
    .bind(now)
    .bind(now)
    .bind(plan_slug.as_deref())
    .execute(&state.pool)
    .await?;
    // First message = description body
    if let Some(desc) = req.description {
        if !desc.trim().is_empty() {
            let mid = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO ticket_messages (id, ticket_id, sender_type, sender_id, body, created_at) VALUES ($1,$2,$3,$4,$5,$6)",
            )
            .bind(mid)
            .bind(id)
            .bind(req.sender_type.as_deref().unwrap_or("agent"))
            .bind(None::<Uuid>)
            .bind(&desc)
            .bind(now)
            .execute(&state.pool)
            .await?;
        }
    }
    let item = sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    // A plan-request ticket is the Plan & Billing screen's request (cron 2026-10-20). While checkout is
    // closed that request IS the buying path, and until now the row was written while the customer's
    // inbox stayed empty. `acknowledged` is reported so the screen can only promise a mail that was
    // actually queued; it is flat in the body so every existing field reads exactly as before.
    //
    // BOTH source names are receipts: the fleet files `plan_request` (IncentiveSwift, WorkflowSwift),
    // and this door filed `billing` until 2026-10-20. Gating on `billing` alone means the day the app
    // files the fleet's own name, the customer stops getting the receipt — a silent regression of the
    // feature this function exists for. `admin_handler::platform_stats` counts both for the same reason.
    let acknowledged = if matches!(item.source.as_str(), "billing" | "plan_request") {
        notify_customer_of_billing_request(&state, &item).await
    } else {
        false
    };
    let mut out = serde_json::to_value(&item).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = out.as_object_mut() {
        obj.insert("acknowledged".to_string(), serde_json::json!(acknowledged));
    }
    Ok(Json(out))
}

/// Tell the CUSTOMER their plan request was received (cron 2026-10-20).
///
/// `POST /api/v1/tickets` with `source: "billing"` is what the Plan & Billing screen files when
/// checkout refuses (this install has no payment provider), so it is the only way a customer can ask
/// to buy. The row was recorded and nothing was sent: they asked to hand over money and heard
/// nothing until an operator happened to answer. Sends through `email::send_template_email` — the ONE
/// funnel carrying per-tenant branding, the tenant's editable template and the reserved-address
/// suppression — so a probe account is recorded `withheld` and no message is attempted.
/// Best-effort by construction: a mail failure is logged and never fails a filed request.
async fn notify_customer_of_billing_request(state: &AppState, ticket: &Ticket) -> bool {
    // A public contact-page row has no workspace, so there is no workspace owner to mail a receipt
    // to — and no plan was asked for either (the public arm never sets `plan_slug`). Nothing to send.
    match ticket.tenant_id {
        Some(tenant_id) => {
            let receipt =
                send_billing_request_receipt(&state.pool, tenant_id, &ticket.subject).await;
            // The operator hears about the SAME ask (2026-11-09): this door is the Plan & Billing
            // screen's request while checkout is closed, so it is the buying path. Best effort —
            // the ticket is already written and a mail failure must not fail the request.
            if let Err(e) = crate::email::send_plan_request_operator_alert(
                &state.pool,
                tenant_id,
                &ticket.subject,
            )
            .await
            {
                tracing::warn!(
                    "tickets door: plan-request operator alert not sent for tenant {tenant_id}: {e}"
                );
            }
            receipt
        }
        None => false,
    }
}

/// The plan-request RECEIPT, reachable from the till as well as from the tickets door.
///
/// Two doors file the same ask: the Tickets screen (`POST /api/v1/tickets`, source `billing` /
/// `plan_request`) and the console's Upgrade button (`POST /api/v1/checkout/create`, which records
/// the row server-side when no provider can charge). Only the FIRST one emailed the customer, so a
/// buyer who pressed Upgrade got a 503 with a reassuring sentence and an empty inbox — while the
/// identical ask filed from the neighbouring screen produced a receipt. Measured 2026-11-05 on a
/// probe workspace: refusal 503, ticket recorded, and exactly one mail row (the signup
/// `welcome_credentials`) — no receipt.
///
/// Takes the pool rather than `AppState` so the checkout route can call it without restructuring
/// its extractors. Best-effort by construction: the row IS the record, and a mail failure must
/// never turn a filed ask into an error. A reserved/harness recipient is withheld by the sender.
pub(crate) async fn send_billing_request_receipt(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    subject: &str,
) -> bool {
    let owner: Option<(Option<String>, String)> = match sqlx::query_as(
        "SELECT name, email FROM users WHERE tenant_id = $1 \n         ORDER BY (role IN ('admin','account_owner')) DESC, created_at ASC LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!("plan-request receipt: owner lookup failed (tenant_id={}): {e}", tenant_id);
            return false;
        }
    };
    let Some((name, email)) = owner else {
        tracing::warn!(
            "plan-request receipt: no user to acknowledge for tenant {}",
            tenant_id
        );
        return false;
    };
    let vars = serde_json::json!({
        "name": name.unwrap_or_else(|| "there".to_string()),
        "request_subject": subject,
        "dashboard_url": "https://app.missedcallrespondr.com/dashboard",
    });
    match crate::email::send_template_email(
        pool,
        tenant_id,
        &email,
        "billing_request_receipt",
        &vars,
    )
    .await
    {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("plan-request receipt to {email} failed (tenant {tenant_id}): {e}");
            false
        }
    }
}

pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let ticket = ticket_visible_to(&state.pool, &claims, id).await?;
    let messages = sqlx::query_as::<_, TicketMessage>(
        "SELECT * FROM ticket_messages WHERE ticket_id = $1 ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(
        serde_json::json!({ "ticket": ticket, "messages": messages }),
    ))
}

pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTicketRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    check_opt_len("status", req.status.as_deref(), max::TICKETS_STATUS)?;
    check_opt_len("priority", req.priority.as_deref(), max::TICKETS_PRIORITY)?;
    let existing = ticket_visible_to(&state.pool, &claims, id).await?;
    let now = chrono::Utc::now();
    let next_status = req.status.as_ref().unwrap_or(&existing.status).clone();
    sqlx::query(
        "UPDATE tickets SET subject=$1, status=$2, priority=$3, assigned_to=$4, contact_id=$5, updated_at=$6 WHERE id=$7",
    )
    .bind(req.subject.as_ref().unwrap_or(&existing.subject))
    .bind(&next_status)
    .bind(req.priority.as_ref().unwrap_or(&existing.priority))
    .bind(req.assigned_to.or(existing.assigned_to))
    .bind(req.contact_id.or(existing.contact_id))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;

    // MOVE THE WORKSPACE ONTO THE PLAN IT ASKED FOR (cron 2026-10-20). Before this an operator's
    // "resolved" changed one word on the row: the customer was told the request was handled while
    // `tenant_plans` still seated the free plan — measured live (filed Pro, resolved, `/auth/me` still
    // answered plan_name "Free"). ONLY an operator can trigger the seat: the customer may also PATCH
    // their own ticket, and letting that arm seat a plan would hand every tenant a free upgrade.
    //
    // Idempotent through `plan_applied_at`, and it seats through `seat_plan_for_tenant` — the same
    // statement the panel's "Assign plan" uses, so the one-row-per-tenant invariant
    // (`tenant_plans_tenant_id_key`) and the credits columns keep their single writer.
    let mut plan_applied = false;
    let mut applied_plan_name: Option<String> = None;
    if caller_is_operator(&claims)
        && matches!(next_status.as_str(), "resolved" | "closed")
        && existing.plan_applied_at.is_none()
    {
        if let (Some(slug), Some(tenant_id)) = (existing.plan_slug.as_deref(), existing.tenant_id) {
            let plan: Option<(Uuid, String)> =
                sqlx::query_as("SELECT id, name FROM plans WHERE slug = $1 AND is_active = true")
                    .bind(slug)
                    .fetch_optional(&state.pool)
                    .await?;
            match plan {
                Some((plan_id, plan_name)) => {
                    crate::handlers::plans_handler::seat_plan_for_tenant(
                        &state, tenant_id, plan_id, None,
                    )
                    .await?;
                    sqlx::query("UPDATE tickets SET plan_applied_at = NOW() WHERE id = $1")
                        .bind(id)
                        .execute(&state.pool)
                        .await?;
                    plan_applied = true;
                    applied_plan_name = Some(plan_name);
                    tracing::info!(
                        ticket = %id,
                        tenant = %tenant_id,
                        plan = %slug,
                        "Plan request resolved — the workspace was seated on the requested plan"
                    );
                }
                None => {
                    tracing::warn!(
                        ticket = %id,
                        plan = %slug,
                        "Plan request names a tier that is no longer active — status only, nothing seated"
                    );
                }
            }
        }
    }

    let item = sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    // The row's own fields, unchanged, plus what the resolve actually did — a superset, so every
    // existing caller of this route reads exactly what it read before.
    let mut out = serde_json::to_value(&item).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = out.as_object_mut() {
        obj.insert("plan_applied".to_string(), serde_json::json!(plan_applied));
        obj.insert(
            "plan_name".to_string(),
            serde_json::json!(applied_plan_name),
        );
    }
    Ok(Json(out))
}

pub async fn add_message(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateTicketMessageRequest>,
) -> Result<Json<TicketMessage>, AppError> {
    check_opt_len(
        "sender_type",
        req.sender_type.as_deref(),
        max::TICKET_MESSAGES_SENDER_TYPE,
    )?;
    let ticket = ticket_visible_to(&state.pool, &claims, id).await?;
    // How many messages the thread already held BEFORE this one. `create()` stores the opening
    // description as the first message, so a non-zero count is what separates a genuine REPLY from
    // that description being stored again — without it, filing a ticket would email the customer a
    // "we answered your request" notice about their own words.
    let prior: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM ticket_messages WHERE ticket_id = $1")
            .bind(id)
            .fetch_one(&state.pool)
            .await?;
    let sender_type = req.sender_type.as_deref().unwrap_or("agent");
    let mid = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO ticket_messages (id, ticket_id, sender_type, sender_id, body, created_at) VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(mid)
    .bind(id)
    .bind(sender_type)
    .bind(req.sender_id)
    .bind(&req.body)
    .bind(now)
    .execute(&state.pool)
    .await?;
    let item = sqlx::query_as::<_, TicketMessage>("SELECT * FROM ticket_messages WHERE id = $1")
        .bind(mid)
        .fetch_one(&state.pool)
        .await?;
    // The customer hears when the OPERATOR answers. The customer's own messages never notify: they
    // are the person who just typed, and this tenant's address is the only mailbox involved.
    let customer_sender = matches!(
        sender_type,
        "customer" | "user" | "client" | "business_user"
    );
    if !customer_sender && prior > 0 {
        notify_customer_of_reply(&state, &ticket, &req.body).await;
    }
    Ok(Json(item))
}

/// Tell the CUSTOMER that an operator answered their ticket.
///
/// MEASURED before this existed (2026-10-19): `ticket_messages` was written only by `create()` —
/// the opening description — so neither console could hold a conversation at all, and a customer who
/// asked for a plan change (the Plan & Billing screen files exactly this ticket) had no way to hear
/// back except reloading the screen and guessing whether anything had happened. Every sibling app
/// already tells its customer (ADASwift, FunnelSwift, WorkflowSwift all shipped it), which is what
/// makes this a gap rather than a design choice.
///
/// Sends through `email::send_template_email` — the ONE funnel that carries per-tenant branding, the
/// tenant's editable template and the reserved-address suppression — so a probe account's address is
/// recorded `withheld` in `email_send_log` and no message is attempted. Best-effort by construction:
/// a mail failure is logged and never turns a saved reply into an error the operator sees.
async fn notify_customer_of_reply(state: &AppState, ticket: &Ticket, body: &str) {
    // WHO hears about the answer, and under which wording (2026-11-06).
    //
    // A ticket filed from inside a workspace is answered to that workspace's owner — the lookup that
    // existed before this. A ticket filed from the PUBLIC contact page has no workspace at all: its
    // address is the one the visitor typed, and the notice must not tell them to open a dashboard
    // they cannot reach, so it uses `support_reply` instead of `ticket_reply`. Without this arm the
    // operator's answer was written to `ticket_messages` and the visitor heard nothing — the exact
    // dead end the contact page was built to close.
    let (name, email, template_type) = match ticket.tenant_id {
        Some(tenant_id) => {
            let owner: Option<(Option<String>, String)> = match sqlx::query_as(
                "SELECT name, email FROM users WHERE tenant_id = $1 \n         ORDER BY (role IN ('admin','account_owner')) DESC, created_at ASC LIMIT 1",
            )
            .bind(tenant_id)
            .fetch_optional(&state.pool)
            .await
            {
                Ok(row) => row,
                Err(e) => {
                    tracing::error!(
                        "tickets.add_message: owner lookup failed (tenant_id={tenant_id}): {e}"
                    );
                    return;
                }
            };
            match owner {
                Some((name, email)) => (
                    name.unwrap_or_else(|| "there".to_string()),
                    email,
                    "ticket_reply",
                ),
                None => {
                    tracing::warn!("tickets.add_message: no user to notify for tenant {tenant_id}");
                    return;
                }
            }
        }
        None => match ticket
            .requester_email
            .as_deref()
            .map(str::trim)
            .filter(|a| !a.is_empty())
        {
            Some(email) => ("there".to_string(), email.to_string(), "support_reply"),
            None => {
                tracing::warn!(
                    "tickets.add_message: public ticket {} carries no requester address to answer",
                    ticket.id
                );
                return;
            }
        },
    };
    let vars = serde_json::json!({
        "name": name,
        "ticket_subject": ticket.subject,
        "reply_body": body,
        "dashboard_url": "https://app.missedcallrespondr.com/dashboard",
    });
    // A public row has no tenant id; a NIL id resolves no stored branding, so the mail carries this
    // app's own wordmark and name (kanban t_6a8af451) and the provider is resolved install-wide.
    if let Err(e) = crate::email::send_template_email(
        &state.pool,
        ticket.tenant_id.unwrap_or_default(),
        &email,
        template_type,
        &vars,
    )
    .await
    {
        tracing::warn!(
            "tickets.add_message: reply notice to {email} failed (ticket {}): {e}",
            ticket.id
        );
    }
}

pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let ticket = ticket_visible_to(&state.pool, &claims, id).await?;
    let result = sqlx::query("DELETE FROM tickets WHERE id = $1")
        .bind(ticket.id)
        .execute(&state.pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Ticket not found".into()));
    }
    Ok(Json(serde_json::json!({"deleted": true, "id": id})))
}

/// Record the paid tier a visitor PICKED on the public pricing page, at the moment they sign up
/// (cron 2026-10-23). The marketing pricing cards already carry the tier into `/register/?plan=…`,
/// and that page showed "You picked Pro" — then posted name+email only, so the pick died at the door
/// and nothing in the operator's console knew a paid tier had been asked for.
///
/// The row is the SAME `tickets` shape the dashboard's "Request the <plan> plan" button files
/// (`source='plan_request'` + `plan_slug`, deduped on one open request per tier), which is what the
/// operator's Plan Requests queue reads and what its Resolve action switches the workspace with. So a
/// signup pick is workable, not a line in a log. Best-effort by design: an unknown, retired or free
/// slug returns `None` and the signup still succeeds — a preference must never refuse an account.
pub async fn record_signup_plan_request(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    picked: &str,
) -> Result<Option<String>, AppError> {
    let slug: String = picked
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if slug.is_empty() || slug.len() > 60 || slug == "free" {
        return Ok(None);
    }
    // Only an ACTIVE, priced tier is a request an operator can act on: `pro` is a retired duplicate
    // of `pro-monthly` here (is_active=false), so a stale link can never file a dead tier.
    let plan: Option<(String, f64)> = sqlx::query_as(
        "SELECT name, COALESCE(NULLIF(price_monthly, 0), price)::float8 \
           FROM plans WHERE slug = $1 AND is_active = true",
    )
    .bind(&slug)
    .fetch_optional(db)
    .await?;
    let Some((name, price)) = plan else {
        return Ok(None);
    };
    if price <= 0.0 {
        return Ok(None);
    }
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tickets \
          WHERE tenant_id = $1 AND source = 'plan_request' \
            AND status IN ('open','in_progress') \
            AND plan_slug IS NOT DISTINCT FROM $2 \
          ORDER BY created_at DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(&slug)
    .fetch_optional(db)
    .await?;
    if existing.is_some() {
        return Ok(Some(name));
    }
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    sqlx::query(
        "INSERT INTO tickets (id, tenant_id, subject, status, priority, assigned_to, contact_id, \
                              source, created_at, updated_at, plan_slug) \
         VALUES ($1,$2,$3,'open','medium',NULL,NULL,'plan_request',$4,$4,$5)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(format!("Plan request at signup: {name} plan"))
    .bind(now)
    .bind(&slug)
    .execute(db)
    .await?;
    sqlx::query(
        "INSERT INTO ticket_messages (id, ticket_id, sender_type, sender_id, body, created_at) \
         VALUES ($1,$2,'agent',NULL,$3,$4)",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(format!(
        "This workspace picked the {name} plan (${price:.0}/month) on the pricing page before it \
         signed up. New accounts start on Free and no card was taken, so the switch has to be made \
         by the team. Filed automatically by the signup door."
    ))
    .bind(now)
    .execute(db)
    .await?;
    // Tell the OPERATOR, not only the customer (2026-11-09). The row above is the buying path on an
    // installation with no payment provider; an ask nobody is told about is a sale nobody makes.
    // Best effort: the ticket is already written, so a mail refusal must not fail the signup.
    if let Err(e) =
        crate::email::send_plan_request_operator_alert(db, tenant_id, &format!("{name} plan")).await
    {
        tracing::warn!(
            "plan request at signup for tenant {tenant_id}: operator alert not sent: {e}"
        );
    }
    Ok(Some(name))
}
