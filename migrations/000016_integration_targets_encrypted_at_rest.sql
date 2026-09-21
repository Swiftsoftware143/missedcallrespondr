-- 000016_integration_targets_encrypted_at_rest.sql
--
-- integration_targets.api_key holds a CUSTOMER-SUPPLIED credential for an outbound integration
-- target (the key the app presents to that target). Before this change
-- src/handlers/integration_target_handler.rs bound the raw request body value straight into the
-- column on both the create and the update path, so the credential sat in the clear at rest and
-- fell out of any database dump or leaked backup.
--
-- Worse than the sibling defect in provider_keys: the same handler also echoed the stored value
-- back to the caller on list, create and update, so a stored secret was additionally a
-- response-body leak. The read paths now return a mask computed from the DECRYPTED value and
-- never the key and never the ciphertext.
--
-- The app now encrypts before it writes, through the module this repo already has
-- (src/security/provider_key_crypto.rs): AES-256 via pgcrypto, master key held ONLY in the
-- process environment (PROVIDER_KEY_ENC_SECRET), stored as 'enc:v1:' plus single-line base64
-- ciphertext. Writes fail closed when the master key is missing or too short, so a plaintext
-- credential is never stored as a fallback.
--
-- This constraint is the regression guard, deliberately identical in shape to the provider_keys
-- guard from 000015: a future writer that forgets to encrypt FAILS CLOSED at the database instead
-- of silently storing a plaintext credential. NULL and the empty string stay allowed because the
-- column is nullable and an absent credential is representable.
--
-- NOT VALID by design: rows written before today (legacy plaintext) would be exempt so the app
-- keeps reading them, while every NEW insert or update is checked. The table held 0 rows when this
-- shipped, so nothing was grandfathered and the constraint is validated after the deploy with
--     ALTER TABLE integration_targets VALIDATE CONSTRAINT integration_targets_api_key_encrypted
-- NOTE: this file is re-run on every boot (src/db.rs::run_migrations), and the DROP/ADD pair below
-- resets that validated flag on each boot - exactly like the provider_keys guard. New writes stay
-- enforced either way, which is what the guard is for.
--
-- Both statements below are independently valid and idempotent, so a re-run is harmless.

CREATE EXTENSION IF NOT EXISTS pgcrypto;

ALTER TABLE integration_targets DROP CONSTRAINT IF EXISTS integration_targets_api_key_encrypted;

ALTER TABLE integration_targets ADD CONSTRAINT integration_targets_api_key_encrypted CHECK (api_key IS NULL OR api_key = '' OR api_key LIKE 'enc:v1:%') NOT VALID;
