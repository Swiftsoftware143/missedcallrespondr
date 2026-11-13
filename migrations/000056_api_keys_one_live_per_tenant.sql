-- 000056 — the API-key contract's one-live-key-per-workspace guarantee (kanban, CoreSwift
-- API-key contract parity). A PARTIAL unique index over (tenant_id) WHERE revoked_at IS NULL:
-- any number of REVOKED rows may share a tenant (the audit history stays), but at most ONE
-- live key can exist per workspace. This is the store-level backstop behind the handler's own
-- 409 refusal on POST /api/v1/api-keys — the handler gives the customer a sentence, the index
-- makes the rule true under concurrency.
--
-- Measured before this file shipped: 6 live keys across 9 tenants, zero duplicate live
-- (tenant_id) pairs, so the index creates cleanly with no data repair.
--
-- Idempotent (CREATE UNIQUE INDEX IF NOT EXISTS), because this runner re-executes every entry
-- on every boot and keeps no ledger.
--
-- Deliberately NOT a constraint on the table definition: a plain UNIQUE (tenant_id) would
-- forbid keeping the revoked history that `revoke` leaves behind (revoke never DELETEs — the
-- row survives with `revoked_at` set). Only the partial predicate expresses the real rule.

CREATE UNIQUE INDEX IF NOT EXISTS api_keys_one_live_per_tenant
    ON public.api_keys (tenant_id)
    WHERE revoked_at IS NULL;
