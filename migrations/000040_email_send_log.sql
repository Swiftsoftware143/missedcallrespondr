-- 000040_email_send_log.sql — the HISTORY behind "did that credentials mail go out?"
--
-- `admin_settings.email_last_send` keeps ONE last-send record and every later mail overwrites it.
-- So a welcome or credentials mail the provider refused at signup is erased the moment any other
-- message succeeds, and the Email Provider panel can only ever answer "what happened to the LAST
-- one?". An operator asking "did this customer ever get their password?" had nothing to read.
--
-- This table is append-only and written from the ONE place every send outcome is recorded
-- (`email::record_last_send`), so any send path added later inherits it. `withheld` is a
-- first-class status, not a failure: a reserved/harness recipient is never attempted at all (no
-- provider request), and recording that as a refusal would be a misreport.
--
-- Idempotent: this app's boot runner re-reads ./migrations on every start with no ledger, so the
-- file must be safe to re-run.
CREATE TABLE IF NOT EXISTS email_send_log (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    to_email   text        NOT NULL,
    template   text,
    provider   text,
    status     text        NOT NULL CHECK (status IN ('sent', 'failed', 'withheld')),
    detail     text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS email_send_log_created_idx ON email_send_log (created_at DESC);
