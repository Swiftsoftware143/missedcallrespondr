-- 000_baseline_live_schema.sql — bootstrap baseline: the relations (and the one column) that THIS
-- database carries and NO migration ever creates, so a fresh install can reach the live schema.
-- Card t_17cef2e9 (found while wiring the from-zero harness into a scheduled guard, t_05c4f080).
--
-- WHY THIS FILE EXISTS
--   src/db.rs::run_migrations is a HARDCODED `include_str!` array applied in ARRAY order, and
--   src/main.rs:23 propagates its error (`run_migrations(&pool).await?`), so a file that fails stops
--   the process before it binds a port. Measured on an empty database 2026-09-26
--   (`fromzero-baseline.sh missedcall` -> rc 3 APPLY-FAIL, 15/20 files applied):
--       000008_credit_system.sql:2        relation "tenant_plans" does not exist
--       000010_payment_provider.sql:3     relation "plans" does not exist
--       000011_schema_fix.sql:33          relation "plans" does not exist
--       000012_coreswift_integration.sql:46  relation "campaigns" does not exist
--       000018_funnelswift_tenant.sql:41  column "is_active" of relation "tenants" does not exist
--   `plans`, `tenant_plans` and `admin_settings` exist in production with NO migration creating any
--   of them, and `tenants.is_active` likewise — that is ONE missing baseline, not five defects. The
--   other two failures are the cascade: `campaigns` (and the 15 further tables) are created by
--   000011/000012/000012's own CREATE TABLE statements, which simply never ran because the roots
--   were missing. Measured: of the 19 live tables a from-zero build lacks, 16 come from those two
--   files and only these 3 come from nowhere.
--
-- POSITION — registered SECOND in the include_str! array, immediately after `000001_initial`
--   Not at the very front: `tenant_plans.tenant_id REFERENCES tenants(id)` and `tenants.is_active`
--   both need `tenants`, which 000001 creates. Before 000001 the FK has nothing to reference on an
--   empty database. Second is late enough — 000002..000007 all applied on an empty database and
--   none of them names `plans`, `tenant_plans` or `admin_settings`; 000008 is the first referrer.
--
-- LIVE SAFETY (this file runs on PRODUCTION at the next boot)
--   Live carries NO migration ledger (`LEDGERS missedcall <none on live>`), so every file re-runs on
--   every boot and each one is written idempotent. This file follows the same rule exactly:
--   `CREATE TABLE IF NOT EXISTS` / `ADD COLUMN IF NOT EXISTS` only; PRIMARY KEY / UNIQUE / FK are
--   declared INLINE so no `ALTER TABLE ... ADD CONSTRAINT` can collide on a re-run; no index is
--   declared separately from the constraint that owns it. All four objects already exist on live,
--   so every statement here is a no-op there. Proof: a `pg_dump` restore of live with this file
--   applied TWICE leaves the whole-catalog md5 and the per-table row fingerprint identical.
--
-- WHAT IS NOT HERE
--   No product rows, and no pricing. `plans` / `tenant_plans` / `feature_limits` rows are the
--   operator's console data (t_7451fc99 precedent: measure the LIVE rows, never seed pricing by
--   hand). This file creates the SHAPE production has and nothing else; 000008 seeds only the Free
--   tier, which is product structure. Row parity is its own question and is reported, not enforced.
--
-- Column order, types, nullability and defaults below are LIVE's, read from the live catalog
-- (`pg_attribute` + `format_type` + `pg_attrdef`). `plans.description` / `price_monthly` /
-- `price_yearly` / `sort_order` (000008) and `payment_provider` (000010) are included because live
-- has them, which makes those ADD COLUMN IF NOT EXISTS statements no-ops on both sides.

CREATE TABLE IF NOT EXISTS plans (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name character varying NOT NULL,
    slug character varying NOT NULL UNIQUE,
    price numeric DEFAULT 0,
    max_leads integer DEFAULT 100,
    max_tags integer DEFAULT 50,
    has_dual_routing boolean DEFAULT false,
    has_multi_tenant boolean DEFAULT false,
    has_white_label boolean DEFAULT false,
    features jsonb DEFAULT '[]'::jsonb,
    created_at timestamp without time zone DEFAULT now(),
    updated_at timestamp without time zone DEFAULT now(),
    purchase_url text,
    payment_provider character varying DEFAULT 'none'::character varying,
    is_active boolean NOT NULL DEFAULT true,
    description text,
    price_monthly numeric(10,2) NOT NULL DEFAULT 0,
    price_yearly numeric(10,2) NOT NULL DEFAULT 0,
    sort_order integer NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS tenant_plans (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    plan_id uuid NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    is_active boolean NOT NULL DEFAULT true,
    created_at timestamp with time zone NOT NULL DEFAULT now(),
    credit_balance integer NOT NULL DEFAULT 0,
    lifetime_credits integer NOT NULL DEFAULT 0,
    status character varying(50) NOT NULL DEFAULT 'active'::character varying,
    billing_cycle character varying(50) NOT NULL DEFAULT 'free'::character varying,
    updated_at timestamp with time zone NOT NULL DEFAULT now(),
    expires_at timestamp with time zone,
    UNIQUE (tenant_id, plan_id)
);

-- Platform-wide key/value settings (panel -> Settings). Read by the admin settings handler.
CREATE TABLE IF NOT EXISTS admin_settings (
    key text PRIMARY KEY,
    value jsonb DEFAULT '{}'::jsonb,
    description text,
    updated_at timestamp with time zone DEFAULT now(),
    updated_by uuid
);

-- `tenants.is_active`: no migration creates it, and 000018_funnelswift_tenant.sql names it in its
-- INSERT, which is why that file died at line 41 on an empty database. Live carries it as
-- NOT NULL DEFAULT true. ADD COLUMN IF NOT EXISTS is a no-op on live and gives a fresh build the
-- column 000018 needs 17 files before 000018 asks for it.
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS is_active boolean NOT NULL DEFAULT true;
