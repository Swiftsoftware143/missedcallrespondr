-- MissedCall Respondr: mark harness-created workspaces at creation (kanban t_4a744754).
--
-- WHY: the public signup (`POST /api/v1/auth/register`) mints a `tenants` row named
-- "<name>'s Workspace" with a slug derived from that name, so a workspace a test harness creates
-- through the real signup door is byte-indistinguishable from a customer's. Attribution must be
-- DATA, not a guess about a name (policy: /opt/swift/docs/fleet-probe-residue-policy-2026-09-28.md,
-- answer 3c; shape proven on FunnelSwift t_fc88ec2a and ADASwift t_ae307966).
--
-- The fleet's harnesses ALREADY send `X-Swift-Harness: <marker>`; this column is what lets the app
-- record it instead of dropping it, so a future sweep over this app is a QUERY rather than a guess.
-- The authoritative detector already implements the marker arm (`scripts/test-tenant-sweep.py`,
-- spec `tag`), and it is the ONLY arm that reaches a workspace whose USER ROW IS ALREADY GONE —
-- the exact shape the sweep's `zero_user_pattern` override had to approximate by name.
--
-- ROOT TABLE: `tenants` — the workspace row `create_account` mints and the row the operator's
-- tenant list shows (`users.tenant_id` -> `tenants.id`). `tenants.slug` carries UNIQUE
-- `tenants_slug_key`; this column is UNRELATED to that index and changes no other writer's shape.
--
-- ADDITIVE, NO BACKFILL — on purpose. The column records where a workspace CAME FROM at creation
-- time and that fact was never captured for the roots that already exist; inventing a value for
-- them would be a lie in the one column a sweeper will trust. Legacy roots stay classified by the
-- owner-ADDRESS policy.
--
-- Idempotent (IF NOT EXISTS + COMMENT), because this app's runner (`src/db.rs::run_migrations`) is
-- an `include_str!` list with NO ledger: every file re-executes in order on EVERY boot.

ALTER TABLE tenants ADD COLUMN IF NOT EXISTS probe_harness text;

COMMENT ON COLUMN tenants.probe_harness IS
  'Harness marker taken from the X-Swift-Harness request header when the workspace was created (trimmed, lowercased, must match ^[a-z0-9][a-z0-9._-]{2,63}$), else NULL. Header only: never set from a request body or query field, and never set by any other route.';
