-- 000065 — CoreSwift's MULTI-KEY rule (CoreSwift migration 207 precedent): a workspace may hold
-- SEVERAL live API keys — the code cap is now 10 — so a second consumer can hold its own labelled
-- key without rotating, and killing, the account's working one. This file DROPS the partial unique
-- index 000056 created (`api_keys_one_live_per_tenant`, `(tenant_id) WHERE revoked_at IS NULL`),
-- which enforced ONE live key per workspace. With the index gone the handler's own 10-live-key
-- count (`handlers::api_key_handler::create_api_key`) is the limit, mirroring CoreSwift's sentence:
--
--   409 — "This account already has 10 active API keys. Revoke one before minting another."
--
-- Idempotent (`DROP INDEX IF EXISTS`), because this runner re-executes every file on every boot and
-- keeps no ledger: 000056 re-creates the index each boot and this file drops it again, so the net
-- schema after every boot is multi-key.
--
-- The fleet-standard mint surface `/api/personal-api-keys` (P1c, spec
-- docs/p1c-personal-api-keys-spec-2026-11-25.md) exposes the same handlers; the existing
-- `/api/v1/api-keys` routes stay untouched for the shipped console.

DROP INDEX IF EXISTS api_keys_one_live_per_tenant;
