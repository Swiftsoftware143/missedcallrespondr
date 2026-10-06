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
        // 000002 (`api_keys`, the table the deleted API-key route group wrote) is RETIRED
        // 2026-10-02 (kanban t_ab963d11) together with its file and its last readers: the routes
        // went with t_f06b1710 (no auth path in this crate ever read the table, so a minted key
        // authenticated nothing), then `features::count_usage`'s api arm and the `max_api_keys`
        // plan row in 000027. A FRESH install must not build a store nothing can name, so the file
        // is gone from disk AND from this list — and 000027 below drops the table in any database
        // that already has it. A database that already has the table loses it; nothing else moved.
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
        // 000018 (`funnelswift_tenant`) was RETIRED 2026-10-01 (kanban t_c2353c90) together with the
        // route it existed for: `POST /api/v1/internal/tag-provision` had no caller (zero readers of
        // `MISSEDCALL_WEBHOOK_URL` anywhere) and the sibling app's push family was removed
        // 2026-09-25, so nothing resolves this slug any more and a fresh database must not seed a
        // tenant no code owns. A database that already has the row keeps it — nothing reads it.
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
        // 000020 restores `tenant_plans_tenant_id_key UNIQUE (tenant_id)`, the constraint the
        // admin panel's own "Assign plan to tenant" action depends on: `admin_assign_plan` upserts
        // `ON CONFLICT (tenant_id)` and the live table had no such unique guard, so the route
        // answered 500 (Postgres 42P10) for EVERY caller, admin included (t_f5494ad5). Registered
        // here so a fresh install gets it too — an unregistered file reaches NO database (see the
        // 000013/000019 notes above). Idempotent: the two data steps are no-ops when a tenant has a
        // single row and the ADD CONSTRAINT is guarded by a pg_constraint check, so the boot-time
        // runner can re-execute it on live safely.
        (
            "000020_tenant_plans_one_row_per_tenant",
            include_str!("../migrations/000020_tenant_plans_one_row_per_tenant.sql"),
        ),
        // 000021 is the store-level half of the email-boundary fix (kanban t_54b1ffab): the column
        // that holds an account's login identity had no format CHECK at all, which is how the literal
        // string `bad` became a real account on live. Guards an existing constraint with a
        // pg_constraint probe, so the boot-time runner re-executing it on live is a no-op.
        (
            "000021_users_email_format_check",
            include_str!("../migrations/000021_users_email_format_check.sql"),
        ),
        // 000022 retires the `max_users` seat cap from the plan data (kanban t_b578b169): the plan
        // model advertised it on Free (1) and Pro Monthly (5) while NO surface can add a user to an
        // existing tenant, so it capped nothing. Its siblings `max_phone_numbers` and `max_tags`
        // were WIRED on their create routes instead and keep their values. Pure data, one
        // idempotent `features - 'max_users'` guarded to object-shaped features, so the boot-time
        // runner re-executing it on live is a no-op after the first pass.
        (
            "000022_retire_max_users_plan_value",
            include_str!("../migrations/000022_retire_max_users_plan_value.sql"),
        ),
        // 000023 stops the `messages.status` column from DEFAULTing to 'sent' (kanban t_4bcf81a8):
        // nothing in this service transmits a message, so the store's own default asserted a
        // delivery for any writer that omitted the column. The one writer now binds 'logged' and
        // leaves sent_at NULL; this removes the landmine for every other writer. One idempotent
        // ALTER, no data statement (messages held 0 rows, and a retroactive relabel could not tell a
        // delivered row from a merely recorded one — the exact claim the card refuses to make).
        (
            "000023_messages_status_logged_default",
            include_str!("../migrations/000023_messages_status_logged_default.sql"),
        ),
        // 000024 adds `messages.provider_message_id` (kanban t_2ed95642): the transport now calls
        // Telnyx's `/v2/messages`, and the only key a later delivery event can be matched by is the
        // provider's own message id. Additive + idempotent (ADD COLUMN IF NOT EXISTS + a PARTIAL
        // unique index, so the many NULLs stay legal).
        (
            "000024_messages_provider_message_id",
            include_str!("../migrations/000024_messages_provider_message_id.sql"),
        ),
        // 000025 adds `response_rules.priority` (kanban t_31f9cf38): the rule evaluator now runs on
        // an inbound call, and the documented evaluation order ("priority, 1 = highest; the first
        // matching rule fires") needed the column it was always described in terms of. Additive +
        // idempotent; DEFAULT 100 is the documented "no preference" position.
        (
            "000025_response_rules_priority",
            include_str!("../migrations/000025_response_rules_priority.sql"),
        ),
        // 000026 retires the DEAD `voicemails` surface (kanban t_1d4fc956): the table (no writer
        // anywhere, 0 rows), its three read routes, and the orphan `inbound_calls.recording_url` /
        // `voicemail_url` / `call_logs.recorded` columns. Pure idempotent DROPs — this runner
        // re-executes every file on every boot, so the second pass is a no-op.
        (
            "000026_retire_voicemails",
            include_str!("../migrations/000026_retire_voicemails.sql"),
        ),
        // 000027 retires the last plan/DB residue of the API-key surface (kanban t_ab963d11): the
        // ONE `feature_limits` row that still sold `max_api_keys` on `enterprise`, and the
        // `api_keys` table itself (0 rows, no writer since t_f06b1710 deleted the routes, no
        // reader once `features::count_usage`'s api arm went, no inbound FK). Pure idempotent
        // DELETE + `DROP TABLE IF EXISTS` — this runner re-executes every file on every boot, so
        // the second pass is a no-op. On a FRESH install the DELETE matches nothing
        // (`000_baseline_live_schema.sql` creates the `feature_limits` SHAPE and seeds no rows —
        // operator data, see its "WHAT IS NOT HERE"), and the DROP is kept ONLY so a fresh build
        // cannot leave behind a table that 000002 used to create on disk.
        (
            "000027_retire_api_keys",
            include_str!("../migrations/000027_retire_api_keys.sql"),
        ),
        // 000028 retires the inert `workflows` module (kanban t_66cfccff): the ONE `feature_limits`
        // row that still sold `max_workflows` on `enterprise`, and both tables of a store nothing
        // evaluates. Pure idempotent DELETE + `DROP TABLE IF EXISTS` — this runner re-executes every
        // file on every boot, so the second pass is a no-op. On a FRESH install the DELETE matches
        // nothing (operator data) and there is nothing to drop, because `000011_schema_fix.sql` no
        // longer creates the tables; the DROP is kept so a database that built them loses them.
        (
            "000028_retire_workflows",
            include_str!("../migrations/000028_retire_workflows.sql"),
        ),
        // 000029 makes the TWO price columns agree (2026-10-03). `plans` kept its price in
        // `price` AND `price_monthly` and each generation of rows filled only one: the Aug-8 trio had
        // `price` (Pro 49, Enterprise 199) with `price_monthly` at 0, while "Pro Monthly" (Aug 19) had
        // `price_monthly` 49 with `price` at 0. The app reads them in DIFFERENT places — the plans list
        // selects `price_monthly::float8`, while the purchase path selects `price::float8` and does
        // `price.unwrap_or(0.0)` — so Pro and Enterprise DISPLAYED $0 and "Pro Monthly" would have
        // CHECKED OUT at $0. One value, two columns, disagreeing.
        //
        // GREATEST(price_monthly, price) into both keeps Free at 0 (both its columns are legitimately 0,
        // so it must not become chargeable) and preserves the 490 yearly already set on Pro Monthly.
        // Idempotent: after the first pass no row matches the WHERE, so the boot-time re-run is a no-op.
        (
            "000029_plans_price_columns_agree",
            include_str!("../migrations/000029_plans_price_columns_agree.sql"),
        ),
        // 000030 retires the user-less HOLDER tenant the deleted tag receiver used to file every
        // FunnelSwift lead into (kanban t_1d08bd9a, `FunnelSwift Leads` / slug `funnelswift` /
        // id 0347dc35-…). 000018 created it and was retired with its handler (t_c2353c90); its row
        // stayed on live because nothing could prove what else pointed at it. This file settles that
        // by measurement: it walks every single-column FK to `tenants` from the CATALOG (plus
        // `email_templates.aid`, the one tenant-ish column with no FK) and deletes the row ONLY when
        // all of them hold zero rows for it, logging the verdict either way. Idempotent by
        // construction — the second pass finds the row absent — and a no-op on a fresh install,
        // which never had it.
        (
            "000030_retire_funnelswift_tenant",
            include_str!("../migrations/000030_retire_funnelswift_tenant.sql"),
        ),
        // 000031 seals the TWO money-bearing credentials in `payment_providers` at rest, the class
        // of kanban t_6104de65: `api_key_encrypted` and `webhook_secret_encrypted` were named after
        // an encryption promise migration 000010 never kept — the upsert bound the raw request
        // value into both, so a Stripe secret key and the endpoint's webhook signing secret sat in
        // the clear. This file arms the two CHECK constraints (sealed-or-empty) so a future writer
        // that forgets to seal FAILS CLOSED. Idempotent because this runner re-executes every file
        // on every boot: the DROP/ADD pair resets the flag and the DO block re-validates it once
        // every row is compliant.
        (
            "000031_payment_providers_secrets_encrypted_at_rest",
            include_str!("../migrations/000031_payment_providers_secrets_encrypted_at_rest.sql"),
        ),
    ];

    for (_name, sql) in migrations {
        sqlx::raw_sql(sql).execute(pool).await?;
    }
    Ok(())
}
