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
-- shipped, so nothing was grandfathered and the self-heal block at the end of this file validates
-- the constraint on the first boot it runs.
--
-- This file is re-run on every boot (src/db.rs::run_migrations), and the DROP/ADD pair below resets
-- that validated flag on each boot - exactly like the provider_keys guard from 000015. The
-- self-heal block therefore re-runs the VALIDATE as soon as every existing row is compliant, so
-- `convalidated = t` ("every row that exists is compliant") is a property the boot path maintains
-- instead of a flag a human has to re-apply after each start. New writes stay enforced either way,
-- which is what the guard is for, and a validation failure is caught as check_violation (WARNING,
-- constraint left NOT VALID, the process still boots). Canonical idiom, with the decision record:
-- /opt/swift/fleet/templates/guard-constraint-not-valid.sql (kanban t_c9b09cc1).
--
-- Every statement below is independently valid and idempotent, so a re-run is harmless.

CREATE EXTENSION IF NOT EXISTS pgcrypto;
ALTER TABLE integration_targets DROP CONSTRAINT IF EXISTS integration_targets_api_key_encrypted;

ALTER TABLE integration_targets ADD CONSTRAINT integration_targets_api_key_encrypted CHECK (api_key IS NULL OR api_key = '' OR api_key LIKE 'enc:v1:%') NOT VALID;

DO $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'integration_targets_api_key_encrypted'
          AND conrelid = 'integration_targets'::regclass
          AND convalidated
    ) THEN
        BEGIN
            ALTER TABLE integration_targets VALIDATE CONSTRAINT integration_targets_api_key_encrypted;
            RAISE NOTICE 'integration_targets.integration_targets_api_key_encrypted validated: every existing row is compliant';
        EXCEPTION
            WHEN check_violation THEN
                RAISE WARNING 'integration_targets.integration_targets_api_key_encrypted still NOT VALID: pre-existing rows violate the guard (backfill them, then re-run this file); new writes are still rejected';
        END;
    END IF;
END
$guard$;
