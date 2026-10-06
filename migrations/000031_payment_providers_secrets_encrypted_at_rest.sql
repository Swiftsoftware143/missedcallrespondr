-- 000031_payment_providers_secrets_encrypted_at_rest.sql
--
-- payment_providers holds TWO CUSTOMER-SUPPLIED credentials, and migration 000010 created them under
-- names that promise encryption at rest ("Encrypted API credentials (encrypted-at-rest via app-layer
-- encryption)"), but nothing ever encrypted them: `upsert_payment_provider` bound the raw request
-- value straight into both columns, so a Stripe secret key (sk_live_...) and the endpoint's webhook
-- signing secret (whsec_...) sat in the clear at rest and would fall out of any database dump,
-- leaked backup or read-only SQL grant.
--
-- WHY THE SIGNING SECRET MATTERS MOST: it is the one credential that decides whether an anonymous
-- POST /api/v1/webhooks/stripe may complete a checkout session and trigger credential delivery.
-- Whoever can read the row can forge a paid event with it.
--
-- The app now seals both before it writes and opens both after it reads: AES-256 via pgcrypto,
-- master key held ONLY in the process environment (PROVIDER_KEY_ENC_SECRET), stored as
-- 'enc:v1:' + base64 ciphertext. See src/security/payment_provider_secrets.rs for the vocabulary
-- and src/security/provider_key_crypto.rs for the format and the fail-closed rule.
--
-- These constraints are the regression guard: a future writer that forgets to seal FAILS CLOSED at
-- the database instead of silently storing a plaintext credential beside sealed ones. An empty
-- string and a NULL stay allowed, because "no credential configured" is representable both ways
-- (000010 declares the columns nullable and the upsert binds '' when a field is not submitted).
-- The publishable_key column is deliberately NOT covered: 000010 stores it in plaintext for
-- frontend use and its name says so.
--
-- This app's runner (src/db.rs::run_migrations) re-executes EVERY file on EVERY boot, so this file
-- must stay idempotent: the DROP/ADD pair below resets the guard each start and the DO block then
-- VALIDATEs it once every existing row is compliant. A row arriving plaintext from a restored dump
-- is exempt (NOT VALID) so the boot still succeeds; the boot half in
-- src/main.rs::seal_legacy_payment_provider_secrets seals such a row in place, after which this
-- block validates on the next start.

ALTER TABLE payment_providers DROP CONSTRAINT IF EXISTS payment_providers_api_key_encrypted;

ALTER TABLE payment_providers ADD CONSTRAINT payment_providers_api_key_encrypted CHECK (api_key_encrypted = '' OR api_key_encrypted LIKE 'enc:v1:%') NOT VALID;

DO $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'payment_providers_api_key_encrypted'
          AND conrelid = 'payment_providers'::regclass
          AND convalidated
    ) THEN
        BEGIN
            ALTER TABLE payment_providers VALIDATE CONSTRAINT payment_providers_api_key_encrypted;
            RAISE NOTICE 'payment_providers.payment_providers_api_key_encrypted validated: every existing row is compliant';
        EXCEPTION
            WHEN check_violation THEN
                RAISE WARNING 'payment_providers.payment_providers_api_key_encrypted still NOT VALID: pre-existing rows violate the guard (the boot half seals them, then this validates on the next start); new writes are still rejected';
        END;
    END IF;
END
$guard$;

ALTER TABLE payment_providers DROP CONSTRAINT IF EXISTS payment_providers_webhook_secret_encrypted;

ALTER TABLE payment_providers ADD CONSTRAINT payment_providers_webhook_secret_encrypted CHECK (webhook_secret_encrypted = '' OR webhook_secret_encrypted LIKE 'enc:v1:%') NOT VALID;

DO $guard$
BEGIN
    IF NOT EXISTS (
        SELECT 1
        FROM pg_constraint
        WHERE conname = 'payment_providers_webhook_secret_encrypted'
          AND conrelid = 'payment_providers'::regclass
          AND convalidated
    ) THEN
        BEGIN
            ALTER TABLE payment_providers VALIDATE CONSTRAINT payment_providers_webhook_secret_encrypted;
            RAISE NOTICE 'payment_providers.payment_providers_webhook_secret_encrypted validated: every existing row is compliant';
        EXCEPTION
            WHEN check_violation THEN
                RAISE WARNING 'payment_providers.payment_providers_webhook_secret_encrypted still NOT VALID: pre-existing rows violate the guard (the boot half seals them, then this validates on the next start); new writes are still rejected';
        END;
    END IF;
END
$guard$;
