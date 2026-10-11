-- 000036_provider_coming_soon.sql
-- MissedCall Respondr: four rows in the integration catalogue are offered to the customer with a key
-- box and ACCEPT a stored credential, but NO code path in this app can ever spend them. A customer
-- pastes a real OpenAI / DeepSeek / Nexweave / SAM.gov key, the app stores it, and nothing ever reads
-- it. Same defect class the ADASwift commit e2e0d35 (kanban t_71794d5a) and the WorkflowSwift 083-087
-- thread closed — this is the MissedCall Respondr half.
--
-- MEASURED ON THE LIVE APP + LIVE DATABASE 2026-10-11 (cron `default`, off-peak; audit dir
-- /opt/swift/audits/t_ada_provider_coming_soon/, kanban t_3bdea5eb):
--   * GET /api/v1/available-providers -> 200, 10 rows; none carried a coming_soon flag (the column
--     did not exist in this database) and all four below were served as connectable.
--   * POST /api/v1/provider-keys against a throwaway tenant -> deepseek 200, openai 200, nexweave 200,
--     sam_gov 200, sendgrid 200 (the control — genuinely spendable by src/email/). i.e. every one of the
--     four ACCEPTED and STORED a credential.
--   * NO READER EXISTS for any of the four. The app's only `provider_keys` readers are
--     telnyx_handler (telnyx / twilio) and the email path (sendgrid / sendiio / mailgun); a repo-wide
--     grep finds the four only in the catalogue seeds (000005 / 000013) plus docs/ADMIN_GUIDE.md.
--     usaspending is NOT in this app's catalogue. A stored key cannot be reached indirectly either —
--     nothing resolves a provider by category.
--
-- WHY coming_soon AND NOT is_active = false, and what it changes:
--   David's standing rule is LABEL, DON'T HIDE. The rows keep is_active = true (still visible in both
--   consoles) and gain coming_soon = true. From this file alone that flag is inert — the two code
--   halves that read it ship in the same commit:
--     * handlers/provider_keys_handler.rs::list_available_providers now serves `coming_soon`
--       (the customer dashboard renders a non-interactive card: a Coming Soon badge, no key input,
--       no Connect / Test button; the operator console labels the four and names the refusal);
--     * handlers/provider_keys_handler.rs::upsert_provider_key now refuses the row with 400
--       "<name> is Coming Soon and cannot be connected yet — nothing in MissedCall Respondr reads its
--       key.", so the API can no longer store a credential nothing can spend.
--
-- REVERSAL (deliberate, one line each): a row returns to connectable with
--   UPDATE available_providers SET coming_soon = false WHERE key = 'openai';
-- and it then needs the missing half named — a real dispatch arm for that provider (an AI / discovery
-- reader) plus whatever credential reader it needs.
--
-- ADDITIVE + IDEMPOTENT: `ADD COLUMN IF NOT EXISTS` is safe on a database that already has the column
-- (a fresh install gets it from this file) and the UPDATE is a no-op once applied. THIS APP'S RUNNER
-- KEEPS NO LEDGER and re-executes every registered file on every boot (src/db.rs include_str! array),
-- so the file is registered there and must stay idempotent. `provider_keys` holds ZERO rows in
-- production, so no customer is affected by the refusal being turned on.

ALTER TABLE available_providers
    ADD COLUMN IF NOT EXISTS coming_soon boolean NOT NULL DEFAULT false;

UPDATE available_providers
   SET coming_soon = true
 WHERE key IN ('openai', 'deepseek', 'nexweave', 'sam_gov');
