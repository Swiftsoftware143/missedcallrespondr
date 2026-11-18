//! `plan_seating` — no workspace is ever left without a plan row.
//!
//! WHY THIS EXISTS (measured 2026-11-14): three workspaces in this app's live database carried no
//! `tenant_plans` row. Every surface that names a level reads `tenant_plans JOIN plans`, so those
//! accounts answered `plan_name: null` to `/auth/me`, painted no plan in the console, and their
//! feature limits silently fell back to free — the operator could not tell what the workspace was
//! on, and the workspace could not either. ADASwift shipped the same repair the same evening
//! (4 rows seated there); this is the port, kept in the app's own schema vocabulary
//! (`plans.is_active`, `tenant_plans.is_active`) rather than copied verbatim from a sibling.
//!
//! Every guarded door already writes the free row inside its own transaction — public signup and
//! the tag provisioning door both resolve the entry plan in-app by slug — so this is the repair for
//! rows minted before those guards, plus the belt for any door that lands a bare workspace later.
//!
//! Only INSERTS where no row exists: an account that already has a plan (free, trial, paid,
//! cancelled) is never touched, so nothing a customer bought can be overwritten.

use sqlx::PgPool;

/// Seat every workspace that has no `tenant_plans` row on this app's own free plan.
///
/// Returns how many rows were written. **Zero is the normal answer** on a database whose accounts
/// all came through the guarded doors.
pub async fn seat_unseated_workspaces(pool: &PgPool) -> Result<u64, sqlx::Error> {
    // The free plan is resolved IN-APP by slug (design §3.1 rule 1) and only when it really is an
    // active, zero-price row — a database whose free plan was renamed or priced out seats nothing
    // rather than inventing a tier.
    //
    // The starter credits travel WITH the seat (measured 2026-11-18): this INSERT used to leave
    // `credit_balance` at its default 0, and the call meter (`telnyx_handler::deduct_credit`)
    // refuses every inbound call unless the balance is positive — so a workspace repaired onto the
    // free plan could not answer a single call. The allowance is read from the plan's OWN `features`
    // JSON (an absent key yields NULL, which is why the fallback is 0 and never an invented number).
    //
    // `ON CONFLICT (tenant_id)` is the second belt: `tenant_plans` is UNIQUE on the workspace, so a
    // race with a concurrent signup loses politely instead of failing the boot.
    let written = sqlx::query(
        r#"INSERT INTO tenant_plans
                  (tenant_id, plan_id, status, billing_cycle, is_active, credit_balance, lifetime_credits)
           SELECT t.id, p.id, 'active', 'free', true,
                  COALESCE(NULLIF(p.features->>'included_credits', '')::int,
                           NULLIF(p.features->>'starter_credits', '')::int, 0),
                  COALESCE(NULLIF(p.features->>'included_credits', '')::int,
                           NULLIF(p.features->>'starter_credits', '')::int, 0)
             FROM tenants t
             JOIN plans p ON p.slug = 'free' AND p.is_active = true AND p.price = 0
            WHERE NOT EXISTS (SELECT 1 FROM tenant_plans tp WHERE tp.tenant_id = t.id)
           ON CONFLICT (tenant_id) DO NOTHING"#,
    )
    .execute(pool)
    .await?;

    Ok(written.rows_affected())
}

/// Boot-time worker. A janitor must never take the app down, so the failure path is a log line.
pub fn spawn_plan_seater(pool: PgPool) {
    tokio::spawn(async move {
        match seat_unseated_workspaces(&pool).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(
                seated = n,
                "plan seating: workspaces that carried no plan row now name the free plan"
            ),
            Err(e) => tracing::warn!(error = %e, "plan seating failed (app keeps serving)"),
        }
    });
}
