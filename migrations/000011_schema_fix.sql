-- ============================================================
-- MissedCall Respondr — Schema Fix (Aug 19 2026)
-- Creates ONLY tables for modules that have NO migration-defined
-- schema (the 9 stub-feature modules + phone_numbers + feature_limits
-- + affiliates). Payment/checkout tables are defined by
-- 000009_payment_checkout.sql and must NOT be duplicated here.
-- Idempotent (IF NOT EXISTS).
-- ============================================================

-- ── Phone Numbers (Telnyx) ───────────────────────────────
CREATE TABLE IF NOT EXISTS phone_numbers (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id            UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    number               VARCHAR(32) NOT NULL,
    friendly_name        VARCHAR(255) DEFAULT '',
    provider             VARCHAR(64) NOT NULL DEFAULT 'telnyx',
    telnyx_connection_id VARCHAR(128),
    is_active            BOOLEAN NOT NULL DEFAULT true,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(number)
);
CREATE INDEX IF NOT EXISTS idx_phone_numbers_tenant ON phone_numbers(tenant_id);
CREATE INDEX IF NOT EXISTS idx_phone_numbers_active ON phone_numbers(tenant_id, is_active);

-- ── Feature Limits (per-plan limits) ────────────────────
CREATE TABLE IF NOT EXISTS feature_limits (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    plan_id     UUID NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    feature_key VARCHAR(64) NOT NULL,
    limit_value BIGINT NOT NULL DEFAULT -1,
    UNIQUE(plan_id, feature_key)
);
CREATE INDEX IF NOT EXISTS idx_feature_limits_plan ON feature_limits(plan_id);

-- ── Credit columns on tenant_plans (idempotent) ─────────
ALTER TABLE tenant_plans ADD COLUMN IF NOT EXISTS credit_balance INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tenant_plans ADD COLUMN IF NOT EXISTS lifetime_credits INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tenant_plans ADD COLUMN IF NOT EXISTS expires_at TIMESTAMPTZ;

-- ── Affiliates ──────────────────────────────────────────
CREATE TABLE IF NOT EXISTS affiliates (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id       UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id         UUID REFERENCES users(id) ON DELETE CASCADE,
    code            VARCHAR(64) NOT NULL,
    commission_rate NUMERIC(6,4) NOT NULL DEFAULT 0.0000,
    is_active       BOOLEAN NOT NULL DEFAULT true,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(code)
);
CREATE INDEX IF NOT EXISTS idx_affiliates_tenant ON affiliates(tenant_id);

-- ── Leads ───────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS leads (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name        VARCHAR(255) NOT NULL DEFAULT '',
    phone       VARCHAR(32),
    email       VARCHAR(320),
    source      VARCHAR(64) NOT NULL DEFAULT 'call',
    status      VARCHAR(32) NOT NULL DEFAULT 'new',
    notes       TEXT,
    tags        TEXT[] DEFAULT '{}',
    call_id     UUID REFERENCES inbound_calls(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_leads_tenant ON leads(tenant_id);
CREATE INDEX IF NOT EXISTS idx_leads_status ON leads(tenant_id, status);

-- ── Deals ───────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS deals (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name        VARCHAR(255) NOT NULL,
    contact_id  UUID REFERENCES contacts(id) ON DELETE SET NULL,
    lead_id     UUID REFERENCES leads(id) ON DELETE SET NULL,
    value       NUMERIC(12,2) NOT NULL DEFAULT 0,
    stage       VARCHAR(64) NOT NULL DEFAULT 'new',
    probability INTEGER NOT NULL DEFAULT 10,
    expected_close_date TIMESTAMPTZ,
    is_won      BOOLEAN NOT NULL DEFAULT false,
    source      VARCHAR(64) NOT NULL DEFAULT 'call',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_deals_tenant ON deals(tenant_id);
CREATE INDEX IF NOT EXISTS idx_deals_stage ON deals(tenant_id, stage);

-- ── Workflows + steps ───────────────────────────────────
CREATE TABLE IF NOT EXISTS workflows (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name        VARCHAR(255) NOT NULL,
    trigger_event VARCHAR(64) NOT NULL DEFAULT 'missed_call',
    is_active   BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_workflows_tenant ON workflows(tenant_id);
CREATE TABLE IF NOT EXISTS workflow_steps (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workflow_id  UUID NOT NULL REFERENCES workflows(id) ON DELETE CASCADE,
    step_order   INTEGER NOT NULL DEFAULT 0,
    action_type  VARCHAR(32) NOT NULL DEFAULT 'sms',
    action_config JSONB NOT NULL DEFAULT '{}',
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_workflow_steps_wf ON workflow_steps(workflow_id);

-- ── Campaigns ───────────────────────────────────────────
CREATE TABLE IF NOT EXISTS campaigns (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name        VARCHAR(255) NOT NULL,
    kind        VARCHAR(32) NOT NULL DEFAULT 'manual',
    is_active   BOOLEAN NOT NULL DEFAULT true,
    status      VARCHAR(32) NOT NULL DEFAULT 'draft',
    metadata    JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_campaigns_tenant ON campaigns(tenant_id);

-- ── Tickets ─────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS tickets (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    subject    TEXT NOT NULL,
    status     VARCHAR(32) NOT NULL DEFAULT 'open',
    priority   VARCHAR(16) NOT NULL DEFAULT 'medium',
    assigned_to UUID REFERENCES users(id) ON DELETE SET NULL,
    contact_id UUID REFERENCES contacts(id) ON DELETE SET NULL,
    source     VARCHAR(32) NOT NULL DEFAULT 'manual',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_tickets_tenant ON tickets(tenant_id);
CREATE INDEX IF NOT EXISTS idx_tickets_status ON tickets(tenant_id, status);
CREATE TABLE IF NOT EXISTS ticket_messages (
    id        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    ticket_id UUID NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    sender_type VARCHAR(16) NOT NULL DEFAULT 'agent',
    sender_id  UUID,
    body      TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_ticket_msgs_ticket ON ticket_messages(ticket_id);

-- ── Clients ─────────────────────────────────────────────
CREATE TABLE IF NOT EXISTS clients (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name        VARCHAR(255) NOT NULL,
    email       VARCHAR(320),
    phone       VARCHAR(32),
    source      VARCHAR(64) NOT NULL DEFAULT 'manual',
    notes       TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_clients_tenant ON clients(tenant_id);

-- ── Calendar Events ─────────────────────────────────────
CREATE TABLE IF NOT EXISTS calendar_events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    title       VARCHAR(255) NOT NULL,
    description TEXT,
    start_at    TIMESTAMPTZ NOT NULL,
    end_at      TIMESTAMPTZ,
    event_type  VARCHAR(64) NOT NULL DEFAULT 'call',
    contact_id  UUID REFERENCES contacts(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_calendar_events_tenant ON calendar_events(tenant_id, start_at);

-- ── Export Templates ────────────────────────────────────
CREATE TABLE IF NOT EXISTS export_templates (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name       VARCHAR(255) NOT NULL,
    entity     VARCHAR(64) NOT NULL DEFAULT 'contacts',
    format     VARCHAR(16) NOT NULL DEFAULT 'csv',
    columns    JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_export_templates_tenant ON export_templates(tenant_id);

-- ── Import Logs ─────────────────────────────────────────
CREATE TABLE IF NOT EXISTS import_logs (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    entity      VARCHAR(64) NOT NULL DEFAULT 'contacts',
    filename    VARCHAR(512) NOT NULL DEFAULT '',
    status      VARCHAR(32) NOT NULL DEFAULT 'pending',
    total_rows  INTEGER NOT NULL DEFAULT 0,
    inserted    INTEGER NOT NULL DEFAULT 0,
    failed      INTEGER NOT NULL DEFAULT 0,
    error_summary TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_import_logs_tenant ON import_logs(tenant_id);
