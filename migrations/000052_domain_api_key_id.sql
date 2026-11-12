-- 000052_domain_api_key_id.sql
--
-- WHY THIS EXISTS. Migration 025 created `private_email_domains` for the ported private inbox but
-- left out ONE column that the ported code requires: `api_key_id`, the optional pointer at a saved
-- credential in `private_email_api_keys`. Everything else came across, so the omission was invisible
-- until a domain row existed — and then it broke the whole feature, not one screen:
--
--   * `PrivateEmailDomain` (src/private_email/models.rs) has `api_key_id: Option<Uuid>`, and every
--     read is `SELECT * ... ` mapped through `FromRow`, so ANY row present makes the read fail with
--     `ColumnNotFound("api_key_id")` — the app answers 500 "Database error".
--   * `add_mailgun_domain` INSERTs `api_key_id` by name, so adding a domain fails outright.
--   * `provision_mailbox`, `verify_domain`, the inbound webhook's domain lookup and the credentials
--     tab all read that row, so creating an inbox address is impossible too.
--
-- Measured before this file, on a tenant with one domain row present:
--   POST /api/v1/private-email/boxes -> 500 {"error":"Database error"}
--   container log                   -> Database error: ColumnNotFound("api_key_id")
--
-- The column is the same shape CoreSwift has: nullable, `ON DELETE SET NULL`, because deleting a
-- saved credential must not delete the domain that used it — it only leaves the domain without a
-- named key, which the handler already handles (`api_key_id` absent -> fall back to the domain's own
-- ciphertext).
--
-- Additive and idempotent: no existing column is altered and the whole file is a no-op on a second
-- boot. `sqlx::migrate!` records it like any other file, so the checksum is fixed at build time —
-- edit nothing here after the build that embeds it.

ALTER TABLE private_email_domains
    ADD COLUMN IF NOT EXISTS api_key_id UUID
    REFERENCES private_email_api_keys(id) ON DELETE SET NULL;
