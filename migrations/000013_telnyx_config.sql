-- ============================================================
-- MissedCall Respondr — 000013: telnyx_config + available_providers drift fix
-- (Sept 20 2026)
--
-- Found while wiring the admin Operator Console:
--   1. GET/PUT /api/v1/admin/telnyx-config returned HTTP 500 --
--      `relation "telnyx_config" does not exist`. No migration in the repo
--      ever created the table, yet telnyx_handler.rs reads it in four places
--      (get_admin_config, put_admin_config, purchase_number, delete_number),
--      so the admin Telnyx key could never be saved and platform (non-BYOK)
--      number purchase/release was dead.
--   2. The live database held only 1 of the 8 rows that migration 000005
--      seeds into available_providers (tables were created there, the seed
--      INSERT was never applied). As a result POST /api/v1/provider-keys
--      rejected every provider except 'mailgun' ("Unknown provider: X"), and
--      the BYOK path for telnyx -- which provider_keys_handler.rs explicitly
--      gate-checks on `req.provider == "telnyx"` -- was unreachable from the
--      admin UI. Telnyx is the app's telephony provider, so it must be
--      selectable.
--
-- Additive + idempotent: creates the missing table if absent, inserts only
-- missing provider rows (ON CONFLICT DO NOTHING), never modifies existing data.
-- ============================================================

-- 1) telnyx_config — platform-wide Telnyx credentials (admin panel) --------
CREATE TABLE IF NOT EXISTS telnyx_config (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    api_key              TEXT NOT NULL DEFAULT '',
    profile_id           VARCHAR(255),
    messaging_profile_id VARCHAR(255),
    webhook_secret       TEXT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- 2) available_providers — restore the rows declared by 000005 + telnyx ----
INSERT INTO available_providers (key, name, description, icon) VALUES
    ('mailgun',  'Mailgun',   'Transactional email sending',            'mail'),
    ('sendgrid', 'SendGrid',  'Email delivery service',                 'mail'),
    ('sendiio',  'Sendiio',   'Email/SMS campaign delivery',            'mail'),
    ('nexweave', 'Nexweave',  'Personalized video/image generation',    'video'),
    ('sam_gov',  'SAM.gov',   'Federal contracting opportunities',       'shield'),
    ('openai',   'OpenAI',    'GPT models for AI features',             'brain'),
    ('deepseek', 'DeepSeek',  'AI model for content generation',        'brain'),
    ('telnyx',   'Telnyx',    'Bring your own Telnyx account (BYOK): calls, SMS and number management', 'phone')
ON CONFLICT (key) DO NOTHING;

-- 000012's CoreSwift provider seed, re-asserted in case it never landed -----
INSERT INTO available_providers (key, name, description, requires_base_url, requires_metadata, icon)
SELECT
    'coreswift',
    'CoreSwift CRM',
    'Connect a personal CoreSwift API key to push captured leads into your CoreSwift CRM lists. Get your key in CoreSwift → Integration Center.',
    true,
    '[]'::jsonb,
    'link'
WHERE NOT EXISTS (SELECT 1 FROM available_providers WHERE key = 'coreswift');
