-- When each person last signed in, so an operator can see a workspace nobody has touched.
--
-- The Tenants screen already answers "whose workspace is this", "how many people are in it" and
-- "which plan is it on" — but not whether anybody has ever signed in, which is the difference
-- between a customer and an abandoned signup. The other consoles carry this column (FunnelSwift,
-- WorkflowSwift, IncentiveSwift, ADASwift, 2026-10-27); this is the same fact here.
--
-- NULL means "never": every existing row starts unset and the console prints the word "never"
-- rather than a blank cell. Additive and nullable, so no reader changes behaviour.
--
-- Catalog-only and idempotent (`IF NOT EXISTS` twice), because this app's boot runner keeps NO
-- ledger and re-executes every file on each boot.
ALTER TABLE users ADD COLUMN IF NOT EXISTS last_login_at timestamptz;

COMMENT ON COLUMN users.last_login_at IS
    'Instant of the last successful sign-in (written by auth::handlers::login); NULL = never signed in.';

CREATE INDEX IF NOT EXISTS users_tenant_last_login_idx ON users (tenant_id, last_login_at DESC);
