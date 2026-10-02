-- 000020_tenant_plans_one_row_per_tenant.sql
-- Restore the "at most one tenant_plans row per tenant" invariant that this table's own code has
-- always assumed, and that the admin panel's "Assign plan to tenant" action depends on
-- (kanban t_f5494ad5, 2026-10-01).
--
-- THE DEFECT: the live table carries only `tenant_plans_pkey (id)` and
-- `UNIQUE (tenant_id, plan_id)`. There is NO unique constraint on `tenant_id` alone, yet
-- `src/handlers/plans_handler.rs::admin_assign_plan` — the handler behind the panel action
-- POST /api/v1/admin/plans/assign — upserts with `ON CONFLICT (tenant_id)`. That conflict target
-- can never resolve, so the statement aborted with Postgres 42P10 ("there is no unique or
-- exclusion constraint matching the ON CONFLICT specification") and the route answered 500 for
-- EVERY caller, admin included: the panel advertised an action that could not succeed.
--
-- THE INTENT IS ONE ROW PER TENANT (measured, not assumed):
--   * every fleet sibling that carries this table declares it — CoreSwift-CRM
--     (`tenant_plans_tenant_id_key UNIQUE (tenant_id)`), ADASwift
--     (migrations/000007_page_tier_pricing.sql) — and ADASwift's plans_handler binds the SAME
--     `ON CONFLICT (tenant_id)` statement that was copied into this crate, where it works.
--   * this crate's reads assume a single row: `admin_handler::add_credits` SELECTs
--     `credit_balance ... WHERE tenant_id = $1` with no status filter and no ORDER/LIMIT and then
--     UPDATEs every row for that tenant; `telnyx_handler::deduct_credit` UPDATEs
--     `... WHERE tenant_id = $1 RETURNING credit_balance`; `admin_handler::list_tenants` LEFT JOINs
--     tenant_plans and would list one tenant row per plan row; `features::plan_slug` and
--     `provider_keys_handler` both take a single `status = 'active'` row.
--   * measured live before this file: 2 rows for 2 tenants (1 each, both `active`/`free`, 50
--     credits) — the constraint is satisfiable and steps 1-2 below are data no-ops.
--
-- The now-redundant `UNIQUE (tenant_id, plan_id)` is deliberately LEFT IN PLACE: it is implied by
-- the new constraint, the baseline creates it, and dropping it would be a second, unrelated schema
-- change. This file only adds the missing guard.
--
-- NOTE ON THE INSTALL PATH: this app ships no in-app migration runner and has no
-- `_sqlx_migrations` ledger, so this file is what a fresh install/restore applies from zero
-- (checked by scripts/fromzero-baseline.sh) and the identical DDL is applied to the live database
-- out of band. Both sides must carry the same object or the from-zero parity check reports drift.

-- Step 1 — fold any duplicate rows' credits into the surviving row (the newest `active` one) so the
-- row that remains cannot have lost a balance. On the measured live data (1 row per tenant) and on
-- a fresh build this is a no-op: it exists so a host whose history differs cannot wedge the boot.
WITH ranked AS (
    SELECT id,
           tenant_id,
           credit_balance,
           lifetime_credits,
           row_number() OVER (
               PARTITION BY tenant_id
               ORDER BY (status = 'active') DESC, updated_at DESC, created_at DESC, id
           ) AS rn
      FROM tenant_plans
), keep AS (
    SELECT tenant_id, id FROM ranked WHERE rn = 1
), extra AS (
    SELECT r.tenant_id,
           SUM(r.credit_balance)::int   AS lost_credits,
           SUM(r.lifetime_credits)::int AS lost_lifetime
      FROM ranked r
      JOIN keep k ON k.tenant_id = r.tenant_id
     WHERE r.rn > 1
     GROUP BY r.tenant_id
)
UPDATE tenant_plans t
   SET credit_balance   = t.credit_balance   + e.lost_credits,
       lifetime_credits = t.lifetime_credits + e.lost_lifetime,
       updated_at       = now()
  FROM extra e, keep k
 WHERE t.id = k.id
   AND k.tenant_id = e.tenant_id;

-- Step 2 — drop the duplicates whose credits step 1 just folded into the survivor.
WITH ranked AS (
    SELECT id,
           row_number() OVER (
               PARTITION BY tenant_id
               ORDER BY (status = 'active') DESC, updated_at DESC, created_at DESC, id
           ) AS rn
      FROM tenant_plans
)
DELETE FROM tenant_plans t
 USING ranked r
 WHERE t.id = r.id
   AND r.rn > 1;

-- Step 3 — enforce it. GUARDED so a host that already carries the constraint (a restore of a
-- database created after this fix) is not wedged by a duplicate-object error. The constraint NAME
-- matches the fleet sibling CoreSwift-CRM so the two schemas read the same.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_constraint
         WHERE conname = 'tenant_plans_tenant_id_key'
           AND conrelid = 'public.tenant_plans'::regclass
    ) THEN
        ALTER TABLE public.tenant_plans
            ADD CONSTRAINT tenant_plans_tenant_id_key UNIQUE (tenant_id);
    END IF;
END
$$;
