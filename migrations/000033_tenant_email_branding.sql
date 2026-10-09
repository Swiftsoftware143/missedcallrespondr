-- 000033_tenant_email_branding.sql
-- David 2026-10-08 (kanban t_feab8aff, app 5 of 5 of the per-tenant email-branding port): every
-- transactional email this app sends should carry the ACCOUNT's own branding — a logo and a brand
-- display name — settable by the account in its own console, so the mail a business's customers
-- receive looks like that business's mail.
--
-- Two pieces of storage, and only ONE of them is new:
--
--  1. `tenant_settings` key `email_branding` = {"brand_name", "brand_color", "logo_url"}. No DDL:
--     `tenant_settings` is the account's own key/value store (PRIMARY KEY (tenant_id, key), value
--     jsonb, FK to tenants ON DELETE CASCADE), and a jsonb document is exactly the shape it already
--     holds. An account with no row is simply unbranded and gets byte-identical mail to before this
--     feature.
--
--  2. `tenant_logos` — the logo's BYTES. This container binds only its release binary and
--     `migrations/` (`docker inspect missedcallrespondr`), so a file written at run time lives inside
--     the container and dies on the next force-recreate, and no host webroot serves it. The bytes are
--     kept in the database and streamed back by `GET /api/v1/branding/logo/:tenant_id`.
--
-- One row per account: the logo is the tenant's, not per-user and not per-app, and the upload path
-- upserts it. Deleting the tenant takes the logo with it (FK ON DELETE CASCADE); the primary key is
-- the only lookup path, so no extra index is needed.
--
-- Idempotent: the boot runner re-executes every migration on each start, so CREATE ... IF NOT EXISTS
-- is required. CREATE TABLE IF NOT EXISTS is safe to re-run; there is no DROP and no data write.
CREATE TABLE IF NOT EXISTS tenant_logos (
    tenant_id    uuid         PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    content_type varchar(100) NOT NULL,
    bytes        bytea        NOT NULL,
    updated_at   timestamp    NOT NULL DEFAULT NOW()
);
