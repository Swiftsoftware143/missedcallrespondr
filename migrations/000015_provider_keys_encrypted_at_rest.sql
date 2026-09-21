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
-- reading them, while every NEW insert/update is checked. From the moment the constraint exists a
-- plaintext credential can no longer be stored, so the guard is live immediately.
--
-- NOT VALID is paired with the self-heal block at the end of this file. This file is re-run on
-- every boot (src/db.rs::run_migrations) and the DROP/ADD pair below resets the flag each time, so
-- the block re-runs the VALIDATE as soon as every existing row is compliant: `convalidated = t`
-- ("every row that exists is compliant") is then a property the boot path maintains, not something
-- a human has to remember to run after the backfill. A validation failure is caught as
-- check_violation, which leaves the constraint NOT VALID with a WARNING and still lets the process
-- boot — never an outage, never a silently dropped guard. Canonical idiom, with the decision
-- record: /opt/swift/fleet/templates/guard-constraint-not-valid.sql (kanban t_c9b09cc1).
--
-- Every statement below is independently valid and idempotent, so a re-run is harmless.

CREATE EXTENSION IF NOT EXISTS pgcrypto;
ALTER TABLE provider_keys DROP CONSTRAINT IF EXISTS provider_keys_api_key_encrypted;

ALTER TABLE provider_keys ADD CONSTRAINT provider_keys_api_key_encrypted CHECK (api_key = '' OR api_key LIKE 'enc:v1:%') NOT VALID;

DO $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'provider_keys_api_key_encrypted'
          AND conrelid = 'provider_keys'::regclass
          AND convalidated
    ) THEN
        BEGIN
            ALTER TABLE provider_keys VALIDATE CONSTRAINT provider_keys_api_key_encrypted;
            RAISE NOTICE 'provider_keys.provider_keys_api_key_encrypted validated: every existing row is compliant';
        EXCEPTION
            WHEN check_violation THEN
                RAISE WARNING 'provider_keys.provider_keys_api_key_encrypted still NOT VALID: pre-existing rows violate the guard (backfill them, then re-run this file); new writes are still rejected';
        END;
    END IF;
END
$guard$;
