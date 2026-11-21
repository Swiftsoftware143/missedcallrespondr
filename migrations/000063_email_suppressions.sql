-- 000063_email_suppressions.sql — a do-not-mail list this app can actually honour (2026-11-21).
--
-- WHY. MissedCall Respondr is the last app in the fleet without a place to record "stop mailing
-- this address". The siblings (FunnelSwift 139, ADASwift 037, IncentiveSwift, WorkflowSwift) all
-- carry one; without it an address that asked to stop — or one the operator watched hard-bounce —
-- keeps receiving mail, which is the fastest way to damage the sending domain (a shared asset for
-- every customer on it).
--
-- SHAPE. Keyed by (tenant_id, address, mail_class) so suppression is per ACCOUNT and per CLASS: one
-- workspace's opt-out never silences another workspace's mail to the same person, and an opt-out
-- from notices never blocks a password reset. `mail_class` is 'notification' for the stoppable class
-- and 'all' for a blanket stop (used when an address hard-bounces).
--
-- Idempotent: guarded so a re-run (this app's runner re-executes every file on each boot) cannot
-- fail on an existing table, and the unique key is declared inside CREATE TABLE.
CREATE TABLE IF NOT EXISTS email_suppressions (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  uuid NOT NULL,
    address    text NOT NULL,
    mail_class text NOT NULL DEFAULT 'notification',
    reason     text NOT NULL DEFAULT '',
    source     text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT email_suppressions_unique UNIQUE (tenant_id, address, mail_class)
);

CREATE INDEX IF NOT EXISTS email_suppressions_lookup
    ON email_suppressions (tenant_id, address);
