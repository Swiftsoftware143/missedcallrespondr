-- 000017_email_templates_schema
--
-- `email_templates` was created out of band: before this file, NO migration in this repo
-- named the table (`grep -rl email_templates migrations/` was empty), so the live table and
-- the Rust code drifted apart. The live table never had `is_html`, while three statements
-- named it:
--   * src/handlers/email_templates_handler.rs `create`  -> live 500 `column "is_html" of
--     relation "email_templates" does not exist`
--   * same file `update`                                -> same 500
--   * src/email.rs (the templated-email lookup)         -> failed behind `.ok().flatten()`,
--     so send_template_email silently used its inline body and never used the DB row
--   (card t_99365fd5)
--
-- This migration makes the repo the owner of the shape. The DDL below is the live table
-- captured from information_schema/pg_indexes on 2026-09-22, plus the `is_html` column the
-- code has always written. Additive and idempotent, so a fresh database now comes up with
-- the table the app actually expects instead of silently missing it.
CREATE TABLE IF NOT EXISTS email_templates (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    template_type text NOT NULL,
    name          text NOT NULL,
    subject       text NOT NULL,
    body          text,
    html_body     text,
    is_html       boolean DEFAULT true,
    is_default    boolean DEFAULT false,
    aid           uuid,
    created_at    timestamp with time zone DEFAULT now(),
    updated_at    timestamp with time zone DEFAULT now()
);

-- The live table was missing this column. DEFAULT true matches the app's own fallback for
-- an absent value (`is_html.unwrap_or(true)`), so the pre-existing template row keeps
-- sending its html_body exactly as it did before this migration.
ALTER TABLE email_templates ADD COLUMN IF NOT EXISTS is_html boolean DEFAULT true;

-- Verbatim from the live table (IF NOT EXISTS, so a no-op there).
CREATE UNIQUE INDEX IF NOT EXISTS idx_email_templates_unique
    ON email_templates (template_type, COALESCE(aid, '00000000-0000-0000-0000-000000000000'::uuid), is_default)
    WHERE aid IS NULL AND is_default = true;
