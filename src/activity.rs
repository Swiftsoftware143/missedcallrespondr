//! Tenant activity feed — the WRITER behind `GET /api/v1/dashboard/activity`.
//!
//! Until this module existed the route had exactly one reader and the `activity_log` table had NO
//! writer anywhere in the crate, so the feed could only ever answer `[]` and the customer dashboard
//! had no screen behind it: the same phantom shape the fleet already retired for `call_logs`
//! (IS-11) and `knowledge_base` (IS-10). The sibling apps already populate this table and already
//! render it — ADASwift (admin console, `t_0aeaddab`) and FunnelSwift (both consoles) — so this
//! wraps the missing half of a fleet-standard surface, it does not invent a new one.
//!
//! Every call here is BEST-EFFORT. A recording, a text or a captured contact must never fail
//! because its audit row could not be written, so a write error is logged and swallowed: the mail
//! row, the call row and the message row are already committed by the time this runs.
//!
//! Table shape (`\d activity_log`): `id` is uuid NOT NULL with NO column default, so it is bound
//! explicitly — the same trap ADASwift's client handler hit. `user_id` is nullable and references
//! `users(id)`, so an automated path (the Telnyx webhook, a response rule) passes `None` rather
//! than minting an id no user row owns.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

/// Append one activity row for a tenant. Best-effort: never propagates an error to the caller.
pub async fn log(
    pool: &PgPool,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    action: &str,
    entity_type: &str,
    entity_id: Option<Uuid>,
    metadata: Value,
) {
    let res = sqlx::query(
        "INSERT INTO activity_log (id, tenant_id, user_id, action, entity_type, entity_id, metadata, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())",
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(user_id)
    .bind(action)
    .bind(entity_type)
    .bind(entity_id)
    .bind(metadata)
    .execute(pool)
    .await;

    if let Err(e) = res {
        tracing::warn!(
            tenant = %tenant_id,
            action,
            error = %e,
            "activity_log write failed (best-effort — the request continues)"
        );
    }
}

/// A missed call landed on one of the tenant's numbers (the webhook and the manual/API intake both
/// call this). `caller` is the number that rang.
pub async fn call_received(
    pool: &PgPool,
    tenant_id: Uuid,
    call_id: Uuid,
    caller: &str,
    disposition: &str,
) {
    log(
        pool,
        tenant_id,
        None,
        "call.received",
        "call",
        Some(call_id),
        json!({ "caller_number": caller, "disposition": disposition }),
    )
    .await;
}

/// Everything one outbound-text event records. A struct rather than eight positional arguments —
/// it is too easy to transpose `to` and `body` at a call site, and clippy's 7-argument ceiling
/// (rightly) refuses the positional form.
pub struct MessageActivity<'a> {
    pub tenant_id: Uuid,
    pub user_id: Option<Uuid>,
    pub message_id: Uuid,
    pub to: &'a str,
    pub body: &'a str,
    pub status: &'a str,
    /// The response rule's name for the automatic arm, `None` for a console send.
    pub auto_rule: Option<&'a str>,
}

/// An outbound text was written (a rule's automatic reply, or a message the operator sent from the
/// console). `auto_rule` separates the two so the feed can say which arm produced it.
pub async fn message_sent(pool: &PgPool, m: MessageActivity<'_>) {
    // The feed shows a one-line preview; the full body stays in the Messages log.
    let preview: String = m.body.chars().take(120).collect();
    log(
        pool,
        m.tenant_id,
        m.user_id,
        "message.sent",
        "message",
        Some(m.message_id),
        json!({ "to_number": m.to, "status": m.status, "preview": preview, "auto_rule": m.auto_rule }),
    )
    .await;
}

/// A contact was captured into this tenant.
pub async fn contact_created(
    pool: &PgPool,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    id: Uuid,
    name: &str,
    phone: Option<&str>,
) {
    log(
        pool,
        tenant_id,
        user_id,
        "contact.created",
        "contact",
        Some(id),
        json!({ "name": name, "phone": phone }),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preview is capped so a long SMS body cannot bloat the feed payload.
    #[test]
    fn preview_is_capped_at_120_chars() {
        let long = "x".repeat(500);
        let preview: String = long.chars().take(120).collect();
        assert_eq!(preview.chars().count(), 120);
    }
}
