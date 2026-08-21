-- ============================================================
-- MissedCall Respondr — CoreSwift Integration + Light Lead Lists
-- (Aug 21 2026)
--
-- Part of David's platform rule: CoreSwift CRM is the backend/system of
-- record for all top-of-funnel tools (MissedCall, FunnelSwift, IncentiveSwift),
-- integrated Zapier-style with a deep layer — each campaign connects to a
-- CoreSwift list; tags propagate so everything stays neatly organized.
--
-- This migration:
--   1. Seeds `coreswift` into available_providers (so users can store their
--      personal CoreSwift API key via POST /api/v1/provider-keys, the same
--      pattern IncentiveSwift uses).
--   2. Creates the `lists` table — each campaign owns its own fresh list
--      (named after the campaign, e.g. campaign "Inbound Plumbing" -> list
--      "Inbound Plumbing"). Standalone lists are also allowed for ad-hoc
--      organization.
--
-- Campaign -> CoreSwift list + MissedCall tag links are stored in the existing
-- `campaigns.metadata` jsonb (keys: metadata.coreswift.list_id,
-- metadata.coreswift.tag_id) — no new columns needed on campaigns.
--
-- Idempotent (IF NOT EXISTS / ON CONFLICT DO NOTHING).
-- ============================================================

-- 1) Seed coreswift as a connectable provider ----------------------------
INSERT INTO available_providers (key, name, description, requires_base_url, requires_metadata, icon)
SELECT
    'coreswift',
    'CoreSwift CRM',
    'Connect a personal CoreSwift API key to push captured leads into your CoreSwift CRM lists. Get your key in CoreSwift → Integration Center.',
    true,             -- requires_base_url: user can set the CoreSwift URL
    '[]'::jsonb,
    '🔗'
WHERE NOT EXISTS (SELECT 1 FROM available_providers WHERE key = 'coreswift');

-- 2) Lists table (each campaign owns its own fresh list) -----------------
CREATE TABLE IF NOT EXISTS lists (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name         VARCHAR(255) NOT NULL,
    campaign_id  UUID REFERENCES campaigns(id) ON DELETE SET NULL,  -- null = standalone list
    description  TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_lists_tenant ON lists(tenant_id);
CREATE INDEX IF NOT EXISTS idx_lists_campaign ON lists(campaign_id);

-- 3) Lead <-> list membership (which leads are in a campaign's list) ------
CREATE TABLE IF NOT EXISTS list_leads (
    list_id    UUID NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    lead_id    UUID NOT NULL REFERENCES leads(id) ON DELETE CASCADE,
    added_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (list_id, lead_id)
);

CREATE INDEX IF NOT EXISTS idx_list_leads_lead ON list_leads(lead_id);
