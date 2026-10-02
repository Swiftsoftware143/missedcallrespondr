-- kanban t_1d4fc956 RETIRE-VOICEMAILS — retire the DEAD `voicemails` surface and its orphan columns.
--
-- MEASURED before this file existed (deployed binary 82addb6e1c0460c4):
--   * `grep -rn 'INSERT INTO voicemails' src/` -> 0 hits anywhere;
--     `SELECT count(*) FROM voicemails` -> 0 (and `inbound_calls` -> 0 too: the app has never taken a call).
--   * the Telnyx webhook (src/handlers/telnyx_handler.rs) handled ONLY `call_received` /
--     `call_initiated` and acked every other event — including Telnyx `call.recording.saved` — with
--     `{"commands":[]}`, so the `record_start` it returned on the handled arm was never captured and
--     `call_logs.recorded` was hardcoded `false`.
--   * there is NO speech-to-text integration anywhere in the crate, so `transcription` could only ever
--     be filled by hand through `PUT /api/v1/voicemails/:id` — over an always-empty table.
--   * no served surface read it: 0 hits for `voicemail` in www-app/ (the tenant console) and www/.
--
-- DECISION (arm B — retire, by measurement, not taste). The WIRE arm was rejected: capturing a
-- recording is a real Telnyx integration, "voicemail transcription" additionally needs an STT provider
-- this app does not have, the schema has no transcription-status vocabulary, and there is no screen —
-- so wiring means inventing a provider and a feature for a surface no tenant can currently reach.
-- The three read routes (`GET /api/v1/voicemails`, `GET|PUT /api/v1/voicemails/:id`,
-- `GET /api/v1/calls/:id/voicemail`) are removed in src/routes.rs, together with the handler module,
-- the model and the `dashboard` voicemail counter.
--
-- This runner (src/db.rs) re-executes EVERY file on EVERY boot and keeps no ledger, so every
-- statement here is idempotent by construction: `DROP ... IF EXISTS` only.
DROP TABLE IF EXISTS voicemails;

-- Orphan columns: written by nobody, read by nothing.
--   `inbound_calls.recording_url` / `inbound_calls.voicemail_url` — the create/update API accepted them
--     as pass-through fields, no caller ever set them (0 non-NULL rows) and no front-end read them;
--   `call_logs.recorded` — its only writer hardcoded `false` (telnyx_handler) and no surface read it,
--     so it asserted a recording that cannot exist.
ALTER TABLE inbound_calls DROP COLUMN IF EXISTS recording_url;
ALTER TABLE inbound_calls DROP COLUMN IF EXISTS voicemail_url;
ALTER TABLE call_logs DROP COLUMN IF EXISTS recorded;
