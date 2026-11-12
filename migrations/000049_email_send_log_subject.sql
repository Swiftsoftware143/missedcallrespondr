-- 000049_email_send_log_subject.sql — keep the SUBJECT of every mail the sender records.
--
-- WHY: `email_send_log` could answer "a mail of kind X went to Y" but never "under what subject", so
-- an operator reading the history after a hand-move could not confirm the customer was told WHICH
-- plan they moved to (measured 2026-11-12 on the operator's assign route). FunnelSwift's log carries
-- the subject, this app's did not.
--
-- Additive and nullable: every existing row keeps NULL and a caller that knows no subject is
-- unaffected. The sender writes it on every attempt, withheld ones included.
ALTER TABLE email_send_log ADD COLUMN IF NOT EXISTS subject text;
