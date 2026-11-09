//! `probe_sweep` — retire harness workspaces that nothing can sign into.
//!
//! Ported from FunnelSwift's sweeper (kanban cron 2026-11-09). Every fleet harness mints a REAL
//! workspace through the real signup route and marks it `probe_harness` (see
//! [`crate::probe_harness`]); the harness then retires its identity but not the workspace, so probe
//! rows accumulate for ever on this app too — measured 2026-11-09: this database carries eight
//! harness-tagged workspaces and there was nothing to retire them. FunnelSwift had the sweeper and
//! this app did not, which is the whole defect: the marker was stamped, the retention was missing.
//!
//! The retention rule is the fleet's own probe-residue policy
//! (`/opt/swift/docs/fleet-probe-residue-policy-2026-09-28.md`, answer 3c): a workspace is retired
//! only when `probe_harness IS NOT NULL` — the marker is stamped from a request HEADER at signup, is
//! NULL for every real signup, and cannot be set from a customer-facing form — AND it is older than
//! [`RETENTION_DAYS`]. Fourteen days is deliberately generous: a harness that is mid-run, or a
//! teammate demonstrating the product, cannot lose a workspace it is still using.
//!
//! Two address classes are never touched even when tagged, because over-protection is the safe
//! direction: the fleet's own provider identity (`swiftsoftware143@…`, and `certifiedtb143@…`), and
//! anything on a fleet-owned REAL domain (`swiftsoftware.com/.net`, `missedcallrespondr.com`) — a
//! workspace there can be a human's.
//!
//! Deleting the workspace row cascades its users, contacts, calls, rules, templates, follow-ups and
//! settings (the FK rules were read from live on 2026-11-09: 49 constraints carry
//! `delete_rule='CASCADE'`), so one statement retires the whole workspace rather than leaving
//! orphans behind.

use sqlx::PgPool;
use std::time::Duration;

/// How often the sweeper looks. Six hours is cheap and bounds how long a stale workspace lives.
pub const SWEEP_INTERVAL_SECS: u64 = 6 * 60 * 60;

/// Age below which a tagged workspace is never considered. Matches the fleet residue policy.
pub const RETENTION_DAYS: i64 = 14;

/// Rows per sweep, so one transaction never holds a long lock on `tenants`.
pub const SWEEP_BATCH: i64 = 50;

/// Boot-time worker, started from `main` after the pool connects. A failure inside one sweep is
/// logged and the loop continues — a janitor must never take the app down.
pub fn spawn_probe_sweeper(pool: PgPool) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(SWEEP_INTERVAL_SECS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            // The first tick fires immediately, so a deploy retires the backlog without waiting
            // six hours.
            ticker.tick().await;
            match sweep_once(&pool).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(retired = n, "probe workspace sweep"),
                Err(e) => tracing::warn!(error = %e, "probe workspace sweep failed"),
            }
        }
    });
}

/// Retires at most [`SWEEP_BATCH`] expired harness workspaces and returns how many went.
///
/// The protected rows are excluded in SQL rather than in Rust so the predicate the operator reads
/// here is exactly the predicate the database applies.
pub async fn sweep_once(pool: &PgPool) -> Result<usize, sqlx::Error> {
    let retired: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        r#"
        DELETE FROM tenants t
         WHERE t.id IN (
                 SELECT c.id FROM tenants c
                  WHERE c.probe_harness IS NOT NULL
                    AND c.created_at < now() - make_interval(days => $1::int)
                    -- never the fleet's own provider identity, and never a fleet-owned REAL
                    -- domain: those can be a human's workspace (residue policy, FINDING 2).
                    AND NOT EXISTS (
                          SELECT 1 FROM users u
                           WHERE u.tenant_id = c.id
                             AND (lower(u.email) LIKE 'swiftsoftware143%'
                               OR lower(u.email) LIKE 'certifiedtb143%'
                               OR lower(u.email) LIKE '%@swiftsoftware.com'
                               OR lower(u.email) LIKE '%@swiftsoftware.net'
                               OR lower(u.email) LIKE '%@missedcallrespondr.com'))
                  ORDER BY c.created_at ASC
                  LIMIT $2)
        RETURNING t.id, t.name
        "#,
    )
    .bind(RETENTION_DAYS)
    .bind(SWEEP_BATCH)
    .fetch_all(pool)
    .await?;

    for (id, name) in &retired {
        tracing::info!(tenant = %id, name = %name, "retired expired probe workspace");
    }
    Ok(retired.len())
}
