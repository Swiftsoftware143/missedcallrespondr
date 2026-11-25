-- 000070_mcp_parity_archived_at.sql — soft-archive columns for the agent-parity families
-- (kanban t_8df5d174, docs/agent-parity-2026-11-25.md MissedCall table).
--
-- The parity card closes CRUD tool families for seven more tenant-scoped entities that carry
-- NO `is_active` flag and no lifecycle `status` column: leads, lists, clients, deals,
-- calendar_events, message_templates and contact_custom_fields. The fleet write contract archives
-- by default — `is_active=false` where the column exists, else the replay-safe `archived_at`
-- pattern already used for `contacts`/`follow_ups` (000069). These seven are the "neither" class,
-- so each gains one nullable `archived_at` and its archive tool sets it; the read tool excludes
-- archived rows unless `include_archived=true`.
--
-- `tickets` is deliberately NOT here: its lifecycle is the `status` column, and a customer may not
-- set it to resolved/closed (`refuse_customer_close`, 2026-11-18) — the door carries no operator
-- role, so a ticket archive tool would widen a rule the console itself enforces. Reported as a gap.
--
-- IDEMPOTENT on purpose: this app replays EVERY migrations/*.sql on every boot with NO ledger
-- (src/db.rs is a hardcoded `include_str!` array), so every statement must be a no-op on a re-run
-- and must not add a UNIQUE/PK index that existing live data could violate. `ADD COLUMN IF NOT
-- EXISTS` is exactly that: nullable, no constraint, no index, safe on live and on a fresh build.
ALTER TABLE leads ADD COLUMN IF NOT EXISTS archived_at timestamptz;
ALTER TABLE lists ADD COLUMN IF NOT EXISTS archived_at timestamptz;
ALTER TABLE clients ADD COLUMN IF NOT EXISTS archived_at timestamptz;
ALTER TABLE deals ADD COLUMN IF NOT EXISTS archived_at timestamptz;
ALTER TABLE calendar_events ADD COLUMN IF NOT EXISTS archived_at timestamptz;
ALTER TABLE message_templates ADD COLUMN IF NOT EXISTS archived_at timestamptz;
ALTER TABLE contact_custom_fields ADD COLUMN IF NOT EXISTS archived_at timestamptz;
