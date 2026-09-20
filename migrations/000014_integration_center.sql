-- ============================================================
-- MissedCall Respondr — 000014: Integration Center catalogue + CoreSwift preset
-- (Sept 20 2026)
--
-- Fleet standard: /opt/swift/docs/integration-center-standard-2026-09-20.md
-- (R1 every app has an Integration Center, R2 every app has a NATIVE CoreSwift
--  integration whose data flows DOWNWARD into CoreSwift).
--
--   1. Assert the canonical `coreswift` row. 000012/000013 seeded it with
--      requires_base_url = TRUE, which contradicts the fleet standard
--      (requires_base_url = FALSE, description = 'Push leads into CoreSwift CRM')
--      and blocks the header-free BYOK connect flow the other spokes ship.
--      Base URL is resolved in code (provider_keys.base_url -> preset -> constant),
--      so the user is not asked for it.
--   2. Complete the catalogue for THIS app's domain: telephony (Telnyx, Twilio) +
--      email. Telnyx/mailgun/sendgrid/sendiio already exist; Twilio is missing.
--   3. `integration_provider_presets` — step 2 of the documented base-URL
--      resolution order, with the coreswift preset row (http://localhost:8084,
--      the crm-swift container) so no spoke has to hardcode the hub URL.
--
-- Idempotent (IF NOT EXISTS / ON CONFLICT) — src/db.rs::run_migrations re-runs
-- every registered file on each boot.
-- ============================================================

-- 1) Canonical CoreSwift catalogue row (identical in every spoke) -------------
INSERT INTO available_providers (key, name, description, requires_base_url, requires_metadata, icon)
VALUES ('coreswift', 'CoreSwift CRM', 'Push leads into CoreSwift CRM', false, '[]'::jsonb, '🔗')
ON CONFLICT (key) DO NOTHING;

UPDATE available_providers
SET name = 'CoreSwift CRM',
    description = 'Push leads into CoreSwift CRM',
    requires_base_url = false,
    requires_metadata = '[]'::jsonb
WHERE key = 'coreswift';

-- 2) Telephony + email catalogue (this app's own domain) ---------------------
INSERT INTO available_providers (key, name, description, requires_base_url, requires_metadata, icon) VALUES
    ('telnyx',  'Telnyx',  'Bring your own Telnyx account (BYOK): calls, SMS and number management', false, '[]'::jsonb, 'phone'),
    ('twilio',  'Twilio',  'Bring your own Twilio account (BYOK): calls, SMS and number management', false, '[]'::jsonb, 'phone'),
    ('mailgun', 'Mailgun', 'Transactional email sending', false, '[]'::jsonb, 'mail'),
    ('sendgrid','SendGrid','Email delivery service',      false, '[]'::jsonb, 'mail'),
    ('sendiio', 'Sendiio', 'Email/SMS campaign delivery', false, '[]'::jsonb, 'mail')
ON CONFLICT (key) DO NOTHING;

-- 3) Provider presets (base-URL resolution step 2) ---------------------------
CREATE TABLE IF NOT EXISTS integration_provider_presets (
    key        TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    base_url   TEXT NOT NULL DEFAULT '',
    docs_url   TEXT,
    sort_order INTEGER NOT NULL DEFAULT 0,
    is_active  BOOLEAN NOT NULL DEFAULT true
);

INSERT INTO integration_provider_presets (key, name, base_url, docs_url, sort_order, is_active)
VALUES
    ('coreswift', 'CoreSwift CRM', 'http://localhost:8084', 'https://coreswiftcrm.com/docs', -10, true),
    ('telnyx',    'Telnyx',        'https://api.telnyx.com/v2', 'https://developers.telnyx.com', 0, true),
    ('twilio',    'Twilio',        'https://api.twilio.com/2010-04-01', 'https://www.twilio.com/docs', 0, true)
ON CONFLICT (key) DO UPDATE
SET name = EXCLUDED.name,
    base_url = EXCLUDED.base_url,
    docs_url = EXCLUDED.docs_url,
    is_active = true;
