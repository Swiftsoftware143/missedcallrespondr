-- 000069_contacts_follow_ups_archived_at.sql — soft archive columns for the two MCP-writable
-- entities that carry no `is_active` flag (kanban t_bb4d4d33, full read+write MCP access).
--
-- The fleet write contract (docs/mcp-write-access-2026-11-25.md §4) archives by default:
--   * a table with `is_active` -> set it false;
--   * a table with a lifecycle `status` -> set the archived status;
--   * NEITHER -> add `archived_at timestamptz`, the archive tool sets it, and that table's
--     list/get statements exclude archived rows unless `include_archived=true`.
--
-- `contacts` and `follow_ups` are the "neither" class, so each gains one nullable `archived_at`.
-- The door's `missedcall_archive_contact` / `missedcall_archive_followup` set it; the existing read
-- tools exclude archived rows by default.
--
-- IDEMPOTENT on purpose: this app replays EVERY migrations/*.sql on every boot with NO ledger
-- (src/db.rs is a hardcoded `include_str!` array), so the statement must be a no-op on a re-run and
-- must not add a UNIQUE/PK index that existing live data could violate. `ADD COLUMN IF NOT EXISTS`
-- is exactly that: nullable, no constraint, no index, safe on live and on a fresh build.
ALTER TABLE contacts ADD COLUMN IF NOT EXISTS archived_at timestamptz;
ALTER TABLE follow_ups ADD COLUMN IF NOT EXISTS archived_at timestamptz;
