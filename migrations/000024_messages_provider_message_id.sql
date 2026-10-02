-- 000024 — the messages row keeps the PROVIDER'S OWN message id (kanban t_2ed95642).
--
-- Wiring the real transport means the send call gets an answer from Telnyx, and the delivery
-- events that arrive later are keyed by the provider's message id (`data.payload.id` on
-- `message.sent` / `message.finalized`). Without that key a webhook event has no way to find the
-- row it belongs to, so `sent_at` / `delivered_at` could never be set from anything but the send
-- request — exactly the claim-without-evidence this card removes. 255 chars, matching the other
-- provider-id columns in this schema (see t_c30d5d52 on bounded client strings).
--
-- Additive + idempotent: a NULL id means "no provider reference" (the inbound arm and every row
-- recorded before this migration), and the unique index is PARTIAL so many NULLs remain legal.
ALTER TABLE messages ADD COLUMN IF NOT EXISTS provider_message_id VARCHAR(255);

CREATE UNIQUE INDEX IF NOT EXISTS messages_provider_message_id_key
    ON messages (provider_message_id)
    WHERE provider_message_id IS NOT NULL;
