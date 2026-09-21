-- 000015_provider_keys_encrypted_at_rest.sql
--
-- provider_keys.api_key holds CUSTOMER-SUPPLIED third-party credentials (CoreSwift personal
-- keys, OpenAI, Resend / SendGrid, Mailgun, social tokens). Before this change the canonical
-- write path bound the raw request value straight into the column, so every BYOK key a customer
-- entered sat in the clear at rest and would fall out of any database dump or leaked backup.
-- Reads were masked, so the exposure was never a response body.
--
-- The app now encrypts before it writes: AES-256 via pgcrypto, master key held ONLY in the
-- process environment (PROVIDER_KEY_ENC_SECRET), stored as 'enc:v1:' + single-line base64
-- ciphertext. See src/security/provider_key_crypto.rs for the format and the fail-closed rule.
--
-- This constraint is the regression guard: a future writer that forgets to encrypt FAILS CLOSED
-- at the database instead of silently storing a plaintext credential. An empty string stays
-- allowed so an empty slot is still representable.
--
-- NOT VALID by design: rows written before today (legacy plaintext) are exempt so the app keeps
-- reading them, while every NEW insert/update is checked. After the one-off backfill the
-- constraint is validated once with
--     ALTER TABLE provider_keys VALIDATE CONSTRAINT provider_keys_api_key_encrypted
--
-- Both statements below are independently valid and idempotent, so a re-run is harmless.

CREATE EXTENSION IF NOT EXISTS pgcrypto;

ALTER TABLE provider_keys DROP CONSTRAINT IF EXISTS provider_keys_api_key_encrypted;

ALTER TABLE provider_keys ADD CONSTRAINT provider_keys_api_key_encrypted CHECK (api_key = '' OR api_key LIKE 'enc:v1:%') NOT VALID;
