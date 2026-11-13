-- 000055 — API keys: the durable bearer credential MissedCall Respondr issues to every account
-- (ADASwift shape, kanban t_b6d650d6; this app's RE-introduction after the 000027 retirement).
--
-- David's standing requirement: EVERY account auto-mints an API key at creation (see
-- auth::signup::create_account), the key stays RETRIEVABLE (key_hash for auth + key_sealed for
-- display, never hash-only), and the settings screen offers View / Copy / Regenerate with an
-- audit row per reveal.
--
-- The key is stored twice:
--   * key_hash   — hex SHA-256, the ONLY thing the auth middleware and the fleet MCP read;
--   * key_sealed — the same key sealed with this app's provider-key envelope (`enc:v1:…`,
--                  pgp_sym_encrypt under PROVIDER_KEY_ENC_SECRET), used ONLY to display it back
--                  to its owner. Authentication never decrypts; reveal never authenticates.
--
-- No `role` column, deliberately: the key's authority is DERIVED from its owning user's live
-- `users.role` at resolve time (handlers::api_key_handler::resolve_api_key), so a demoted owner's
-- key loses access immediately and a deleted owner's key dies with them — the same boundary the
-- JWT path already enforces in auth::middleware. The fleet MCP resolver (mcp/src/main.rs) also
-- reads only `key_hash` / `tenant_id` / `key_prefix` / `revoked_at`.
--
-- Idempotent by construction (this runner re-executes every file per boot): CREATE TABLE IF NOT
-- EXISTS + CREATE INDEX IF NOT EXISTS + DROP CONSTRAINT IF EXISTS before ADD CONSTRAINT.
-- NOTE: 000027_retire_api_keys.sql used to DROP this table; that DROP was removed when this file
-- reintroduced it, because a re-executed DROP would wipe every key on each boot.

CREATE TABLE IF NOT EXISTS public.api_keys (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    uuid NOT NULL REFERENCES public.tenants(id) ON DELETE CASCADE,
    name         text NOT NULL,
    key_prefix   varchar(16) NOT NULL,
    key_hash     varchar(64) NOT NULL UNIQUE,
    key_sealed   text,
    created_by   uuid,
    created_at   timestamp without time zone NOT NULL DEFAULT now(),
    last_used_at timestamp without time zone,
    revoked_at   timestamp without time zone
);

CREATE INDEX IF NOT EXISTS idx_api_keys_tenant ON public.api_keys (tenant_id);

ALTER TABLE public.api_keys DROP CONSTRAINT IF EXISTS api_keys_key_sealed_envelope;
ALTER TABLE public.api_keys ADD CONSTRAINT api_keys_key_sealed_envelope
    CHECK (key_sealed IS NULL OR key_sealed LIKE 'enc:v1:%');
