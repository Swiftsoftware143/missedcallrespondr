-- 000044 — a workspace must be DELETABLE. Three foreign keys on `users` fall outside every
-- cascade, so the operator's "delete workspace" was refused and the workspace stayed.
--
-- MEASURED LIVE 2026-10-20 (probe workspace minted at probe-…@example.invalid, mail suppressed at
-- source, deleted afterwards): `POST /api/v1/admin/tenants/bulk-delete` answered
--   {"deleted":0,"deleted_ids":[],"failed":[{"error":"error returned from database: update or
--    delete on table \"users\" violates foreign key constraint \"password_resets_user_id_fkey\"
--    on table \"password_resets\""}]}
-- i.e. a workspace whose owner had ever pressed "Forgot password" could not be removed, and the
-- panel was shown that raw Postgres sentence. The same three keys all have `confdeltype = 'a'`
-- (NO ACTION), so ANY workspace with call activity (`activity_log.user_id`), an assigned
-- follow-up (`follow_ups.assigned_to`) or a password-reset row was equally undeletable — and every
-- real workspace has activity.
--
-- THE RULE CHOSEN, per column, from its own nullability:
--   * `password_resets.user_id` is NOT NULL  -> ON DELETE CASCADE (a reset row for a user that no
--     longer exists is meaningless).
--   * `activity_log.user_id` and `follow_ups.assigned_to` are NULLABLE -> ON DELETE SET NULL, so the
--     call history and the follow-up SURVIVE the user (they are the workspace's operational record,
--     and losing them would be a data loss the operator never asked for).
--
-- Guarded on the CURRENT delete rule, so it is idempotent: this app's runner keeps no ledger and
-- re-executes every registered file on each boot. On a database that already cascades, the DO block
-- is a no-op and re-running costs one catalog read.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_constraint
                WHERE conname = 'password_resets_user_id_fkey' AND confdeltype <> 'c') THEN
        ALTER TABLE password_resets DROP CONSTRAINT password_resets_user_id_fkey;
        ALTER TABLE password_resets ADD CONSTRAINT password_resets_user_id_fkey
            FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE;
    END IF;

    IF EXISTS (SELECT 1 FROM pg_constraint
                WHERE conname = 'activity_log_user_id_fkey' AND confdeltype <> 'n') THEN
        ALTER TABLE activity_log DROP CONSTRAINT activity_log_user_id_fkey;
        ALTER TABLE activity_log ADD CONSTRAINT activity_log_user_id_fkey
            FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE SET NULL;
    END IF;

    IF EXISTS (SELECT 1 FROM pg_constraint
                WHERE conname = 'follow_ups_assigned_to_fkey' AND confdeltype <> 'n') THEN
        ALTER TABLE follow_ups DROP CONSTRAINT follow_ups_assigned_to_fkey;
        ALTER TABLE follow_ups ADD CONSTRAINT follow_ups_assigned_to_fkey
            FOREIGN KEY (assigned_to) REFERENCES users(id) ON DELETE SET NULL;
    END IF;
END $$;
