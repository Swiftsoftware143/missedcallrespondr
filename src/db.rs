use sqlx::PgPool;

/// Runs all migrations in dependency order.
/// Migrations are idempotent (IF NOT EXISTS / ADD COLUMN IF NOT EXISTS).
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::Error> {
    let migrations: &[(&str, &str)] = &[
        (
            "000001_initial",
            include_str!("../migrations/000001_initial.sql"),
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
    ];

    for (_name, sql) in migrations {
        sqlx::raw_sql(sql).execute(pool).await?;
    }
    Ok(())
}
