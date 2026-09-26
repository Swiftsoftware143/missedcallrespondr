use sqlx::PgPool;

/// Runs all migrations in dependency order.
/// Migrations are idempotent (IF NOT EXISTS / ADD COLUMN IF NOT EXISTS).
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    let migrations: &[(&str, &str)] = &[
        (
            "000001_initial",
            include_str!("../migrations/000001_initial.sql"),
        ),
        // The bootstrap baseline, registered SECOND on purpose: it creates the relations that exist
        // on live but that NO migration creates (`plans`, `tenant_plans`, `admin_settings`) plus
        // `tenants.is_active`, and `tenant_plans`' FKs need `tenants` from 000001 above. Without it
        // 000008/000010/000011/000012/000018 fail on an empty database and the process never binds
        // a port (t_17cef2e9). No-op on live — every statement is IF NOT EXISTS. See the file header.
        (
            "000_baseline_live_schema",
            include_str!("../migrations/000_baseline_live_schema.sql"),
        ),
        (
            "000002_api_keys",
            include_str!("../migrations/000002_api_keys.sql"),
        ),
        (
            "000003_portfolio_integrations",
            include_str!("../migrations/000003_portfolio_integrations.sql"),
        ),
        (
            "000004_add_email_description",
            include_str!("../migrations/000004_add_email_description.sql"),
        ),
        (
            "000004_password_resets",
            include_str!("../migrations/000004_password_resets.sql"),
        ),
        (
            "000005_provider_keys",
            include_str!("../migrations/000005_provider_keys.sql"),
        ),
        (
            "000006_campaign_triggers",
            include_str!("../migrations/000006_campaign_triggers.sql"),
        ),
        (
            "000007_contact_custom_fields",
            include_str!("../migrations/000007_contact_custom_fields.sql"),
        ),
        (
            "000008_credit_system",
            include_str!("../migrations/000008_credit_system.sql"),
        ),
        // Note: duplicate 000008_credit_system + 000008_tag_groups_and_tags both executed
        (
            "000008_tag_groups_and_tags",
            include_str!("../migrations/000008_tag_groups_and_tags.sql"),
        ),
        (
            "000009_payment_checkout",
            include_str!("../migrations/000009_payment_checkout.sql"),
        ),
        (
            "000010_payment_provider",
            include_str!("../migrations/000010_payment_provider.sql"),
        ),
        (
            "000011_schema_fix",
            include_str!("../migrations/000011_schema_fix.sql"),
        ),
        (
            "000012_coreswift_integration",
            include_str!("../migrations/000012_coreswift_integration.sql"),
        ),
        // 000013 shipped on 2026-09-20 (telnyx_config + the 000005 provider seed that never
        // landed) but was NEVER registered here, so it was applied by hand and a fresh
        // database would come up without telnyx_config. Registered now — the file is
        // additive + idempotent (IF NOT EXISTS / ON CONFLICT DO NOTHING).
        (
            "000013_telnyx_config",
            include_str!("../migrations/000013_telnyx_config.sql"),
        ),
        (
            "000014_integration_center",
            include_str!("../migrations/000014_integration_center.sql"),
        ),
        (
            "000015_provider_keys_encrypted_at_rest",
            include_str!("../migrations/000015_provider_keys_encrypted_at_rest.sql"),
        ),
        (
            "000016_integration_targets_encrypted_at_rest",
            include_str!("../migrations/000016_integration_targets_encrypted_at_rest.sql"),
        ),
        // 000017 owns the `email_templates` table shape at last: no earlier migration ever
        // named it, so the live table (missing `is_html`) and the code (three statements
        // naming it) had drifted apart. Additive + idempotent (CREATE TABLE IF NOT EXISTS +
        // ADD COLUMN IF NOT EXISTS), so it is a no-op everywhere except for the one missing
        // column, and a fresh database now gets the table instead of silently missing it.
        (
            "000017_email_templates_schema",
            include_str!("../migrations/000017_email_templates_schema.sql"),
        ),
        // 000018 owns the tenant that receives contacts auto-provisioned by the FunnelSwift
        // tag-provision webhook. The handler used to bind a hardcoded tenant uuid that existed in
        // NO database, so every provision of a new email died on contacts_tenant_id_fkey with a
        // 500 (t_c9669881). The handler now resolves its owner by SLUG at runtime; this migration
        // is what makes that slug exist on a fresh database. Idempotent (ON CONFLICT (slug) DO
        // NOTHING), so it is a no-op on a database whose operator already owns that slug.
        (
            "000018_funnelswift_tenant",
            include_str!("../migrations/000018_funnelswift_tenant.sql"),
        ),
        // 000019 shipped 2026-09-26 (t_158bf73d, the Stripe receiver's two refusal arms) and was
        // NEVER registered here, so it reached NO database, fresh or live — the live
        // `payment_webhook_events` was missing the `error_message` column and the status CHECK's
        // two new arms purely because nothing ever ran the file. It is additive + fully idempotent
        // (`ADD COLUMN IF NOT EXISTS` + `DROP CONSTRAINT IF EXISTS` before `ADD CONSTRAINT`), so
        // registering it is a no-op on live except that the two objects start existing, and a fresh
        // build gets them too.
        (
            "000019_payment_webhook_status_arms",
            include_str!("../migrations/000019_payment_webhook_status_arms.sql"),
        ),
    ];

    for (_name, sql) in migrations {
        sqlx::raw_sql(sql).execute(pool).await?;
    }
    Ok(())
}
