use axum::{
    extract::{Extension, Path, Query, State},
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
    pub tenant_id: Uuid,
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
                      COALESCE(tn.name, '') AS tenant_name,
                      COALESCE(u.email, '') AS account_email,
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
    let owner: Option<(Option<String>, String)> = match sqlx::query_as(
        "SELECT name, email FROM users WHERE tenant_id = $1 \n         ORDER BY (role IN ('admin','account_owner')) DESC, created_at ASC LIMIT 1",
    )
    .bind(ticket.tenant_id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(
                "tickets.create: owner lookup failed (tenant_id={}): {e}",
                ticket.tenant_id
            );
            return false;
        }
    };
    let Some((name, email)) = owner else {
        tracing::warn!(
            "tickets.create: no user to acknowledge for tenant {}",
            ticket.tenant_id
        );
        return false;
    };
    let vars = serde_json::json!({
        "name": name.unwrap_or_else(|| "there".to_string()),
        "request_subject": ticket.subject,
        "dashboard_url": "https://app.missedcallrespondr.com/dashboard",
    });
    match crate::email::send_template_email(
        &state.pool,
        ticket.tenant_id,
        &email,
        "billing_request_receipt",
        &vars,
    )
    .await
    {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(
                "tickets.create: plan-request receipt to {email} failed (ticket {}): {e}",
                ticket.id
            );
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
        if let Some(slug) = existing.plan_slug.as_deref() {
            let plan: Option<(Uuid, String)> =
                sqlx::query_as("SELECT id, name FROM plans WHERE slug = $1 AND is_active = true")
                    .bind(slug)
                    .fetch_optional(&state.pool)
                    .await?;
            match plan {
                Some((plan_id, plan_name)) => {
                    crate::handlers::plans_handler::seat_plan_for_tenant(
                        &state,
                        existing.tenant_id,
                        plan_id,
                        None,
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
                        tenant = %existing.tenant_id,
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
    let owner: Option<(Option<String>, String)> = match sqlx::query_as(
        "SELECT name, email FROM users WHERE tenant_id = $1 \n         ORDER BY (role IN ('admin','account_owner')) DESC, created_at ASC LIMIT 1",
    )
    .bind(ticket.tenant_id)
    .fetch_optional(&state.pool)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(
                "tickets.add_message: owner lookup failed (tenant_id={}): {e}",
                ticket.tenant_id
            );
            return;
        }
    };
    let Some((name, email)) = owner else {
        tracing::warn!(
            "tickets.add_message: no user to notify for tenant {}",
            ticket.tenant_id
        );
        return;
    };
    let vars = serde_json::json!({
        "name": name.unwrap_or_else(|| "there".to_string()),
        "ticket_subject": ticket.subject,
        "reply_body": body,
        "dashboard_url": "https://app.missedcallrespondr.com/dashboard",
    });
    if let Err(e) = crate::email::send_template_email(
        &state.pool,
        ticket.tenant_id,
        &email,
        "ticket_reply",
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
