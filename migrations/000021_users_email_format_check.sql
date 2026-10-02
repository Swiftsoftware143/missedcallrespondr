-- 000021_users_email_format_check
--
-- WHY (kanban t_54b1ffab). `public.users.email` is an account's login identity AND the only address
-- its credentials/welcome mail can ever be delivered to. The column was `text NOT NULL UNIQUE` with
-- NO `CHECK`, and no handler in the request path validated the format, so POST /api/v1/auth/register
-- stored the literal string `bad` and minted a real account (tenant 9aa023c1 / user 5cd9750b, both
-- retired under t_a4de4363) that no mail could ever reach — a "silent dead account" created one layer
-- before the credentials-mail transport that t_6d575da6 fixed on the same app.
--
-- The application boundary now refuses that input with 422 before any INSERT
-- (src/security/email_addr.rs::normalize, used by register, forgot-password, the admin portfolio-sync
-- create path and the checkout credential-delivery path). This constraint is the store-level backstop
-- for the writers nobody has written yet — the same "fix the class, not the call site" posture the
-- fleet applies elsewhere.
--
-- The pattern is deliberately LOOSER than the Rust validator so the database can never refuse a value
-- the application accepted: the application additionally rejects whitespace/control characters, empty
-- and dot-only local/domain parts, and over-long addresses. Everything this regex requires — a non-empty
-- part, one `@`, a non-empty dotted domain — is required by the application too. Plus-aliases
-- (`a+b@x.com`), dotted locals (`a.b@x.com`) and IDN domains (`user@münchen.de`) pass both.
--
-- Idempotent for the boot-time runner (src/db.rs re-executes every registered file on every boot):
-- `ADD CONSTRAINT` has no `IF NOT EXISTS`, so it is guarded by a pg_constraint probe. Every live row
-- was checked against the pattern before this constraint was added (4 rows: 0 violations).

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'users_email_format_check'
          AND conrelid = 'public.users'::regclass
    ) THEN
        ALTER TABLE public.users
            ADD CONSTRAINT users_email_format_check
            CHECK (email ~ '^[^[:space:]@]+@[^[:space:]@]+\.[^[:space:]@]+$');
    END IF;
END
$$;
