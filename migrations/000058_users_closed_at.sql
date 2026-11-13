-- 000058_users_closed_at.sql — a customer can close their own account (2026-11-13).
--
-- Ported from FunnelSwift (migration 132), IncentiveSwift and ADASwift, where a customer could
-- already close their own account, while here the only lever was asking support to DELETE the user
-- row. Every foreign key from `users`/`tenants` to the workspace's calls, contacts, follow-ups and
-- recordings CASCADEs, so a deletion performed for a privacy request would have destroyed the
-- business's history as a side effect.
--
-- A closure is therefore a STATE, never a delete:
--   * `closed_at` records the decision so the operator can still see the workspace and its data;
--   * the close handler bumps `sessions_valid_from` in the SAME statement, so every token minted
--     before the closure is refused at its next request;
--   * `login`, `forgot_password` and `reset_password` all refuse a closed account by name, so a
--     reset link cannot quietly reopen it;
--   * the workspace's live API keys are revoked, because those are durable credentials that are
--     never measured against the session line.
-- `is_active` is deliberately left alone: it is the operator's own switch, and a closed customer
-- must stay visible in the operator's people list instead of vanishing from it.
--
-- Idempotent by construction (ADD COLUMN IF NOT EXISTS / CREATE INDEX IF NOT EXISTS), because this
-- app's runner re-executes every migration file on every boot.
ALTER TABLE users ADD COLUMN IF NOT EXISTS closed_at timestamptz;

-- The operator's audits ask "who closed?" — a partial index keeps that read cheap without indexing
-- the hundreds of thousands of live rows.
CREATE INDEX IF NOT EXISTS idx_users_closed_at
    ON users (closed_at) WHERE closed_at IS NOT NULL;
