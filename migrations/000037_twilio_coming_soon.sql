-- 000037_twilio_coming_soon.sql
-- MissedCall Respondr: ONE catalogue row still accepts and stores a customer credential while NO code
-- path in this app can ever spend it — `twilio`. This is the row the 000036 sweep missed.
--
-- MEASURED LIVE 2026-10-11 (cron `default`, off-peak; app on 127.0.0.1:8088):
--   * GET /api/v1/available-providers -> `twilio` present, coming_soon = false, is_active = true. The
--     tenant dashboard (www-app/missedcall/dashboard/index.html) therefore renders a Twilio key box
--     plus Connect and "Test connection", and the marketing page promises "Paste your own API key for
--     any provider in the catalogue — CoreSwift CRM, Telnyx, Twilio, Mailgun, SendGrid …".
--   * POST /api/v1/provider-keys {provider:"twilio", api_key:"…"} -> 200: the credential is STORED.
--   * NO READER EXISTS. The app's ONLY `provider_keys` readers are
--         src/handlers/telnyx_handler.rs   WHERE provider = 'telnyx'      (hard-coded)
--         src/handlers/coreswift_external.rs WHERE provider = 'coreswift'  (hard-coded)
--     and a repo-wide case-insensitive grep finds `twilio` ONLY in the catalogue seed
--     (000014_integration_center.sql), docs / marketing copy and migration comments — ZERO occurrences
--     in src/. It is reachable by no indirect route either: nothing resolves a provider by category.
--
--     (The 000036 header named telnyx_handler as the reader for "telnyx / twilio". It is NOT: the
--     handler binds `provider = 'telnyx'` literally, and `upsert_provider_key`'s BYOK plan gate fires
--     only for `provider == "telnyx"`. So the Twilio row was left connectable on a wrong premise.)
--   * The "Test connection" route (coreswift_integration_handler::test_provider_key) answers
--     "No live connection test available for this provider yet" for anything but `coreswift`, so it
--     cannot spend a Twilio key either.
--
-- WHY coming_soon AND NOT is_active = false (David's standing rule — LABEL, DON'T HIDE): the row keeps
-- is_active = true (still visible in both consoles) and gains coming_soon = true. From this file alone
-- that flag is inert; the two code halves that read it shipped with 000036 and are already live:
--   * handlers/provider_keys_handler.rs::list_available_providers serves `coming_soon` -> the tenant
--     dashboard renders a non-interactive card (Coming Soon badge, no key input, no Connect/Test);
--   * handlers/provider_keys_handler.rs::upsert_provider_key refuses the row with 400
--     "<name> is Coming Soon and cannot be connected yet — nothing in MissedCall Respondr reads its
--     key.", so the API can no longer store a credential nothing can spend.
--
-- REVERSAL (deliberate, one line): UPDATE available_providers SET coming_soon = false WHERE key =
-- 'twilio'; and it then needs the missing half named — a real Twilio dispatch arm (calls / SMS /
-- number management) plus the credential reader that arm needs.
--
-- ADDITIVE + IDEMPOTENT: `ADD COLUMN IF NOT EXISTS` is a no-op on a database that already has the
-- column (000036 added it; kept here so a fresh install is safe regardless of order) and the UPDATE is
-- a no-op once applied. THIS APP'S RUNNER KEEPS NO LEDGER and re-executes every registered file on
-- every boot (src/db.rs include_str! array), so this file is registered there and must stay idempotent.

ALTER TABLE available_providers
    ADD COLUMN IF NOT EXISTS coming_soon boolean NOT NULL DEFAULT false;

UPDATE available_providers
   SET coming_soon = true
 WHERE key = 'twilio';
