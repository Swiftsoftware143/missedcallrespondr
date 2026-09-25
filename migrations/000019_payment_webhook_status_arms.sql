-- 000019: `payment_webhook_events` learns the two refusal arms the Stripe receiver
-- distinguishes, and the reason text for each (kanban t_158bf73d, same class as t_40b77d6a).
--
--   not_configured   the receiver has no signing secret to verify with (no active `stripe` row
--                    in payment_providers, or a row without webhook_secret_encrypted) — a state
--                    an operator fixes from the payment-gateways panel, so the delivery is
--                    answered 503 and Stripe retries it
--   signature_failed a `Stripe-Signature` was presented (or absent) and did not verify
--
-- Measured before writing this (`\d payment_webhook_events`): this deployment had NO
-- `payment_webhook_events_status_check` at all (unlike WorkflowSwift, where migration 060 had to
-- DROP + ADD one) and NO `error_message` column, so the audit row could not record WHY a
-- delivery was refused. Both are added here. Nothing is dropped except the constraint if a
-- previous run of this migration had already created it; no row is rewritten and no column type
-- changes, so the pre-fix binary keeps running unchanged against this schema.
ALTER TABLE payment_webhook_events ADD COLUMN IF NOT EXISTS error_message text;

ALTER TABLE payment_webhook_events DROP CONSTRAINT IF EXISTS payment_webhook_events_status_check;
ALTER TABLE payment_webhook_events ADD CONSTRAINT payment_webhook_events_status_check
  CHECK (status = ANY (ARRAY['received'::text, 'processed'::text, 'failed'::text,
                             'ignored'::text, 'not_configured'::text, 'signature_failed'::text]));
