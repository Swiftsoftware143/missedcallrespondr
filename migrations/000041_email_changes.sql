-- 000041_email_changes.sql — a customer can change their own sign-in address (kanban t_0d1aa081)
--
-- The profile screen showed `users.email` as a DISABLED input and no route anywhere in the app
-- could change it (`grep -rn 'email_change' src/` was 0 hits). Signup collects NAME + EMAIL only,
-- so a single typo at signup left an account whose owner could never receive another message from
-- us — including the next password reset.
--
-- This table is the PENDING half of the fleet's proven two-arm flow (IncentiveSwift t_6b2702e3,
-- carried by FunnelSwift/WorkflowSwift first — copied, not redesigned):
--   * `POST /api/v1/auth/email-change`         writes ONE row and mails a confirmation link to the
--                                              NEW address; `users.email` does not move;
--   * `POST /api/v1/auth/email-change/confirm` redeems the token and moves `users.email`.
--
-- Why a table rather than a token column on `users`: the move must be reversible by simply NOT
-- opening the link, and a superseded request has to keep proving what was asked for. `used` is set
-- by BOTH the redeem path and the retire-on-new-request path, so one live request exists per user
-- and an older link can never be redeemed after a newer one is asked for.
--
-- Column names follow THIS app's vocabulary: the identity row is `users`, so the owner key is
-- `user_id` (IncentiveSwift, whose identity table is `accounts`, carries `account_id` — same
-- contract, same table name, the app's own column for the owner).
--
-- Idempotent: the boot runner in `src/db.rs` re-executes every registered file on every start with
-- no ledger, so each statement must be safe to run against a database that already has the object.
CREATE TABLE IF NOT EXISTS email_changes (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    new_email   text NOT NULL,
    token       text NOT NULL UNIQUE,
    expires_at  timestamptz NOT NULL,
    used        boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- The confirm arm looks a token up by value (covered by the UNIQUE constraint); the request arm
-- retires the caller's outstanding rows, so it wants this partial index rather than a full scan of
-- the table on every profile save.
CREATE INDEX IF NOT EXISTS email_changes_open_by_user
    ON email_changes (user_id) WHERE used = false;

COMMENT ON TABLE email_changes IS
    'Pending sign-in-address changes (kanban t_0d1aa081). One live row per user; redeemed or superseded rows keep used=true.';
