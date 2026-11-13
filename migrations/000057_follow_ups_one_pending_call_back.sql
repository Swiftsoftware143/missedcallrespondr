-- One pending call-back per inbound call.
--
-- WHY. `POST /api/v1/calls/:id/respond` — the "Respond" button on the Calls screen, and the
-- `callback` arm of the response rules — is the product's core action: it queues a call-back
-- follow-up for a missed call. Before this migration the handler INSERTed a fresh row on every
-- press with nothing preventing a second one, so the same caller was filed again and again.
-- Measured live on 2026-11-13 against a probe workspace: three presses of the same call produced
-- three 'pending' call_back rows for ONE missed call, and the dashboard's Follow-ups list showed
-- the same number three times. The UI disables the button for the rest of the page's life, but a
-- reload re-enables it and any API client can repeat the call.
--
-- The handler now answers `already_scheduled` when a pending call-back exists, and this partial
-- unique index is the store-level backstop that makes the rule hold when two requests race
-- (the read-then-insert in the handler is not atomic on its own).
--
-- Idempotent by construction, because this app's runner re-executes every migration file on every
-- boot: the de-duplication below is a fixed point (it keeps the EARLIEST pending row per call and
-- deletes only later duplicates), and the index is `IF NOT EXISTS`. A database that already holds
-- exactly one pending call-back per call is untouched.
--
-- The de-duplication is scoped to call_back/pending rows only. Completed call-backs, other
-- follow-up types, and notes are never removed: it deletes nothing but duplicates of a row the
-- application itself created by mistake.

DELETE FROM follow_ups f
 USING follow_ups g
 WHERE f.follow_type = 'call_back'
   AND g.follow_type = 'call_back'
   AND f.status = 'pending'
   AND g.status = 'pending'
   AND f.call_id = g.call_id
   AND f.id <> g.id
   -- keep the EARLIEST of a duplicate set (created_at first, id breaking an exact tie)
   AND (g.created_at, g.id) < (f.created_at, f.id);

CREATE UNIQUE INDEX IF NOT EXISTS follow_ups_one_pending_call_back
    ON follow_ups (call_id)
 WHERE follow_type = 'call_back' AND status = 'pending';
