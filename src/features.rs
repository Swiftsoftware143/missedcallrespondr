//! Feature limits enforcement — reads limits from plans table
//! (dedicated columns + `features` JSONB) and enforces per-tenant.
//!
//! A tenant with NO active plan is NOT unlimited: it resolves to [`DEFAULT_PLAN_SLUG`], the same
//! tier registration seats a brand-new signup on (kanban t_f6f94b8e — see that constant for the
//! decision, the two rejected arms and the live measurement behind it).
use crate::error::AppError;
use sqlx::PgPool;
use uuid::Uuid;

/// Resolve the tenant's current active plan slug.
async fn plan_slug(pool: &PgPool, tenant_id: Uuid) -> Result<Option<String>, AppError> {
    let slug = sqlx::query_scalar(
        "SELECT p.slug FROM tenant_plans tp JOIN plans p ON p.id = tp.plan_id \
         WHERE tp.tenant_id = $1 AND tp.status = 'active'",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(slug)
}

/// The plan a tenant with NO active `tenant_plans` row is treated as (kanban t_f6f94b8e).
///
/// Registration seats every self-serve signup on this plan (`auth/handlers.rs`: `SELECT id FROM
/// plans WHERE slug = 'free' AND is_active = true`), so it IS this product's default tier. Before
/// this constant existed, `plan_slug()` answered `None` for a tenant with no active plan, both
/// gates below returned `Ok(())`, and "no plan" therefore meant NO ALLOWANCE AT ALL on any of the
/// 15 numeric dimensions `count_usage` knows and on every boolean flag — strictly more than the top tier gets. That
/// was live, not theoretical: 4 of the 6 tenants in the live database carried no `tenant_plans`
/// row (the `funnelswift` provision owner seeded by what was then
/// `migrations/000018_funnelswift_tenant.sql`, plus tenants created outside registration), and
/// `POST /api/v1/internal/tag-provision` answered 201 for the 6th contact on a tenant the free
/// tier caps at 5 — the finding that raised this card. That route, its owner-tenant migration and
/// its `TAG_PROVISION_TENANT_SLUG` were themselves retired as uncalled by kanban t_c2353c90; the
/// decision recorded here stands on its own, and the live `funnelswift` row — owned by no code now
/// — is simply one of the plan-less tenants this floor applies to.
///
/// DECISION (arm (a) of the three weighed): fall back at the READ, inside the one function both
/// gates share, so every dimension, every route and every plan-less tenant move together.
///
/// Arm (b), seating only the seeded owner tenant in an additive migration, was rejected: it leaves
/// the other plan-less tenants unbounded, invents a tier for a tenant that owns a cross-app inbox,
/// and this app re-executes every migration file on boot (there is no tracking table), so it would
/// re-add the row an operator deliberately deleted. Arm (c), recording "no plan = unlimited", was
/// rejected as the hole itself — reachable by any holder of the shared `INTERNAL_SYNC_KEY` and by
/// any churned tenant, and it is what t_f6fdfeee's own live proof had to work around by
/// temporarily seating a plan.
///
/// A tenant with no plan now gets EXACTLY the floor a brand-new signup gets. An operator who wants
/// more assigns a plan, and a real plan always wins over this default (proven live both ways).
pub const DEFAULT_PLAN_SLUG: &str = "free";

/// The tenant's active plan slug, or [`DEFAULT_PLAN_SLUG`] when it has none.
///
/// The `bool` reports that the default was applied, so a refusal can name WHY it happened: an
/// operator reading the log learns the tenant carries no plan instead of guessing which ceiling
/// refused it. An INACTIVE `tenant_plans` row (`status <> 'active'`) means no plan here too — the
/// same rule `plan_slug` always applied.
async fn resolve_plan_slug(pool: &PgPool, tenant_id: Uuid) -> Result<(String, bool), AppError> {
    match plan_slug(pool, tenant_id).await? {
        Some(slug) => Ok((slug, false)),
        None => Ok((DEFAULT_PLAN_SLUG.to_string(), true)),
    }
}

/// WHERE a resolved entitlement came from. The panel's catalogue reports this next to the value,
/// so "what the panel shows" and "what the gate read" cannot diverge: both call `resolve_limit` /
/// `resolve_flag` below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A `feature_limits` row — the panel-managed grant (kanban t_dd2f7e32).
    FeatureLimits,
    /// `plans.features->>key` — a per-plan override written by the panel's raw JSON action.
    FeaturesJson,
    /// A dedicated `plans` column (`max_leads`, `has_white_label`, …).
    Column,
    /// The plan declares nothing — the absence rule decides.
    Unset,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::FeatureLimits => "feature_limits",
            Source::FeaturesJson => "features",
            Source::Column => "column",
            Source::Unset => "unset",
        }
    }
}

/// The panel-managed grant for one plan: `feature_limits.limit_value` (one row per plan × key).
/// Vocabulary (shared with the panel and `feature_registry`): `-1` unlimited / granted,
/// `0` NOT available on this plan, `N > 0` a cap of N. Absence is `None`.
pub async fn entitlement_for_plan(
    pool: &PgPool,
    plan_id: Uuid,
    feature_key: &str,
) -> Result<Option<i64>, AppError> {
    let v: Option<i64> = sqlx::query_scalar(
        "SELECT limit_value FROM feature_limits WHERE plan_id = $1 AND feature_key = $2",
    )
    .bind(plan_id)
    .bind(feature_key)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(v)
}

/// The same lookup by plan slug.
pub async fn entitlement_by_slug(
    pool: &PgPool,
    slug: &str,
    feature_key: &str,
) -> Result<Option<i64>, AppError> {
    let v: Option<i64> = sqlx::query_scalar(
        "SELECT f.limit_value FROM feature_limits f JOIN plans p ON p.id = f.plan_id \
         WHERE p.slug = $1 AND f.feature_key = $2",
    )
    .bind(slug)
    .bind(feature_key)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(v)
}

/// Resolve a NUMERIC limit for a plan, with its source. Order of resolution:
///   1. `feature_limits` (the panel-managed grant — kanban t_dd2f7e32),
///   2. `features->>key` (an explicit per-plan OVERRIDE — see the t_0e61628c note below),
///   3. the dedicated `plans` column for the keys that have one (`max_leads`, `max_tags`),
///   4. `None` = the plan declares nothing → the absence rule (allow) decides.
///
/// The gate (`enforce_feature_limit`) and the admin catalogue both call THIS, which is what makes
/// the panel's reading of a grant the same trade the gate enforced.
pub async fn resolve_limit(
    pool: &PgPool,
    slug: &str,
    feature_key: &str,
) -> Result<(Option<i64>, Source), AppError> {
    if let Some(v) = entitlement_by_slug(pool, slug, feature_key).await? {
        return Ok((Some(v), Source::FeatureLimits));
    }
    if let Some(v) = jsonb_limit(pool, slug, feature_key).await? {
        return Ok((Some(v), Source::FeaturesJson));
    }
    let column_sql = match feature_key {
        "max_leads" | "leads" | "max_contacts" | "contacts" => {
            Some("SELECT max_leads FROM plans WHERE slug = $1")
        }
        "max_tags" | "tags" => Some("SELECT max_tags FROM plans WHERE slug = $1"),
        _ => None,
    };
    if let Some(sql) = column_sql {
        // Dedicated plan columns (max_leads, max_tags) are INT4 (integer).
        if let Some(v) = sqlx::query_scalar::<_, i32>(sql)
            .bind(slug)
            .fetch_optional(pool)
            .await?
        {
            return Ok((Some(v as i64), Source::Column));
        }
    }
    Ok((None, Source::Unset))
}

/// Resolve a BOOLEAN flag for a plan, with its source. Order:
///   1. `feature_limits` (non-zero = granted, `0` = refused — the panel-managed grant),
///   2. `features->>key` (`"true"`/`"1"` = granted, any other PRESENT value = refused),
///   3. the dedicated boolean column when the key has one (`has_dual_routing`, …),
///   4. `None` = unset → the absence rule (REFUSED) decides — which is why a boolean key granted
///      by no plan at all (has_calendar, before this card) refused every tier including the top.
pub async fn resolve_flag(
    pool: &PgPool,
    slug: &str,
    feature_key: &str,
) -> Result<(Option<bool>, Source), AppError> {
    if let Some(v) = entitlement_by_slug(pool, slug, feature_key).await? {
        return Ok((Some(v != 0), Source::FeatureLimits));
    }
    let raw: Option<String> = sqlx::query_scalar("SELECT features->>$1 FROM plans WHERE slug = $2")
        .bind(feature_key)
        .bind(slug)
        .fetch_optional(pool)
        .await?
        .flatten();
    match raw.as_deref() {
        Some("true") | Some("1") => return Ok((Some(true), Source::FeaturesJson)),
        Some(_) => return Ok((Some(false), Source::FeaturesJson)),
        None => {}
    }
    // Fall back to a dedicated boolean column if it exists (dual-routing etc.). Whole query as a
    // compile-time literal — gate 5d, same reason as the numeric arms above.
    let sql = match feature_key {
        "has_dual_routing" => Some("SELECT has_dual_routing FROM plans WHERE slug = $1"),
        "has_multi_tenant" => Some("SELECT has_multi_tenant FROM plans WHERE slug = $1"),
        "has_white_label" => Some("SELECT has_white_label FROM plans WHERE slug = $1"),
        _ => None,
    };
    if let Some(sql) = sql {
        let v: Option<bool> = sqlx::query_scalar(sql)
            .bind(slug)
            .fetch_optional(pool)
            .await?
            .flatten();
        if let Some(v) = v {
            return Ok((Some(v), Source::Column));
        }
    }
    Ok((None, Source::Unset))
}

/// Fetch a numeric limit for a feature_key.
/// Order of resolution (kanban t_0e61628c — one allowance PER DIMENSION, each overridable per plan,
/// then t_dd2f7e32 which put the panel-managed `feature_limits` grant FIRST):
///   1. `feature_limits` — the panel's own grant, so a key the admin assigned is the key the gate
///      reads even when the plan's JSONB says nothing.
///   2. `features->>key` — an explicit per-plan OVERRIDE, and it WINS over the plan's own column. The
///      admin writes it from the panel's own "Set plan features (JSON)" action
///      (PUT /api/v1/admin/plans/:id/features, src/handlers/plans_handler.rs), so a CONTACT allowance
///      (or a leads / tags one) can differ from the plan default with no code change and no schema
///      change. CoreSwift-CRM carries its contact allowance exactly this way: a per-tier
///      `features.max_contacts` (100 / 500 / 1 000 / 5 000 / 10 000 / 50 000).
///   3. the dedicated `plans` column for the keys that have one (`max_leads`, `max_tags`) — the
///      plan's DEFAULT, and where every live plan resolves: no `plans` row in this database carries a
///      `max_contacts` or `max_leads` features key (census in the t_0e61628c evidence), so adding the
///      override arm moved no live number.
///   4. `features->>key` for every other key (max_users, max_rules, max_phone_numbers, ...).
///   5. None = no limit declared → allow.
async fn numeric_limit(
    pool: &PgPool,
    slug: &str,
    feature_key: &str,
) -> Result<Option<i64>, AppError> {
    Ok(resolve_limit(pool, slug, feature_key).await?.0)
}

/// Apply a `features->>key` value as a bigint, or `None` when the plan declares no such key.
///
/// A PRESENT but non-integer value is treated as ABSENT (kanban t_0e61628c). This expression is
/// reachable from the admin panel's own "Set plan features (JSON)" action and
/// `(features->>'max_contacts')::bigint` on a hand-typed value ("unlimited", " 10") aborts the whole
/// statement — the route answered 500 for every request until the value was fixed. A malformed
/// override now leaves the plan's declared limit in place instead of breaking the endpoint.
async fn jsonb_limit(
    pool: &PgPool,
    slug: &str,
    feature_key: &str,
) -> Result<Option<i64>, AppError> {
    let v: Option<i64> = sqlx::query_scalar(
        "SELECT CASE WHEN (features->>$1) ~ '^-?[0-9]+$' THEN (features->>$1)::bigint END \
         FROM plans WHERE slug = $2",
    )
    .bind(feature_key)
    .bind(slug)
    .fetch_optional(pool)
    .await?
    .flatten();
    Ok(v)
}

/// Count current usage for a feature key (per tenant).
///
/// `max_leads` / `leads` count the **leads** table (kanban t_39b9a474). It used to be in the arm
/// above, next to `max_contacts`, so the plan's `max_leads` limit was compared against the CONTACT
/// count and the `leads` table was bounded by nothing (measured on t_92abc097: `max_leads=1` with
/// one lead already present answered 200 CREATED). Each plan dimension counts its own entity — the
/// fleet convention (FunnelSwift `max_leads` -> leads, IncentiveSwift `max_leads` -> leads,
/// CoreSwift-CRM `max_contacts` -> contacts) and the arity of this app's own plan row: the
/// `POST /api/v1/leads` route (src/handlers/leads_handler.rs:81) enforces the key `max_leads` under
/// the label "Leads". The contact arm is left exactly as it was.
///
/// The ALLOWANCE that arm is compared against is its own key now (`features.max_contacts`, kanban
/// t_0e61628c) — see `numeric_limit` above. Counting is unchanged by that card.
async fn count_usage(pool: &PgPool, tenant_id: Uuid, key: &str) -> Result<i64, AppError> {
    let q = match key {
        "max_contacts" | "contacts" => Some("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1"),
        "max_leads" | "leads" => Some("SELECT COUNT(*) FROM leads WHERE tenant_id = $1"),
        "max_tags" | "tags" => Some("SELECT COUNT(*) FROM tags WHERE tenant_id = $1"),
        "max_phone_numbers" | "phone_numbers" => {
            Some("SELECT COUNT(*) FROM phone_numbers WHERE tenant_id = $1")
        }
        "max_rules" | "rules" => Some("SELECT COUNT(*) FROM response_rules WHERE tenant_id = $1"),
        // `users` has NO `is_active` column in this schema (measured: information_schema). The
        // reference was wrong in both directions: here it made the `max_users` arm a SQL ERROR
        // (a 500 where a 402 belongs, reachable the moment a route gates `max_users` — and the
        // default-plan fallback above routes plan-less tenants through this very arm, since `free`
        // declares `features.max_users`), and in `get_usage_json` below `unwrap_or(0)` hid it.
        "max_users" | "users" => Some("SELECT COUNT(*) FROM users WHERE tenant_id = $1"),
        "max_deals" | "deals" => Some("SELECT COUNT(*) FROM deals WHERE tenant_id = $1"),
        "max_workflows" | "workflows" => {
            Some("SELECT COUNT(*) FROM workflows WHERE tenant_id = $1")
        }
        "max_campaigns" | "campaigns" => {
            Some("SELECT COUNT(*) FROM campaigns WHERE tenant_id = $1")
        }
        "max_tickets" | "tickets" => Some("SELECT COUNT(*) FROM tickets WHERE tenant_id = $1"),
        "max_follow_ups" | "follow_ups" => {
            Some("SELECT COUNT(*) FROM follow_ups WHERE tenant_id = $1")
        }
        "max_messages" | "messages" => Some("SELECT COUNT(*) FROM messages WHERE tenant_id = $1"),
        "max_integrations" | "integrations" => {
            Some("SELECT COUNT(*) FROM integrations WHERE tenant_id = $1")
        }
        "max_api_keys" | "api_keys" => Some("SELECT COUNT(*) FROM api_keys WHERE tenant_id = $1"),
        "max_calls" | "calls" => Some("SELECT COUNT(*) FROM inbound_calls WHERE tenant_id = $1"),
        _ => None,
    };
    match q {
        Some(sql) => Ok(sqlx::query_scalar(sql)
            .bind(tenant_id)
            .fetch_one(pool)
            .await?),
        None => Ok(0),
    }
}

/// Say WHY a refusal happened when the tenant carried no plan at all: without this line an
/// operator sees a 402 for a tenant the panel shows as having no plan and has to guess that the
/// gate fell back to [`DEFAULT_PLAN_SLUG`]. Silent by design when a real plan refused — that
/// tenant's ceiling is already visible on its plan row.
fn warn_defaulted(defaulted: bool, tenant_id: Uuid, slug: &str, key: &str, reason: &str) {
    if defaulted {
        tracing::warn!(
            "feature_limit: tenant {} has NO active plan — the default plan '{}' applies: key={} refused ({})",
            tenant_id,
            slug,
            key,
            reason
        );
    }
}

/// Enforce a numeric (max_*) feature limit.
pub async fn enforce_feature_limit(
    pool: &PgPool,
    tenant_id: Uuid,
    feature_key: &str,
    label: &str,
) -> Result<(), AppError> {
    // NO ACTIVE PLAN IS NOT "NO ALLOWANCE" (kanban t_f6f94b8e): the tenant resolves to the
    // default plan, i.e. the floor a brand-new signup gets, instead of being allowed everything.
    let (slug, defaulted) = resolve_plan_slug(pool, tenant_id).await?;
    let limit = match numeric_limit(pool, &slug, feature_key).await? {
        Some(l) => l,
        None => return Ok(()), // the plan declares no such limit → allow
    };
    // -1 (or any negative) = unlimited
    if limit < 0 {
        return Ok(());
    }
    // 0 = feature not included on this plan
    if limit == 0 {
        warn_defaulted(
            defaulted,
            tenant_id,
            &slug,
            feature_key,
            "not available on it",
        );
        return Err(AppError::UpgradeRequired(format!(
            "{} is not available on your current plan. Upgrade to access this feature.",
            label
        )));
    }
    let usage = count_usage(pool, tenant_id, feature_key).await?;
    if usage >= limit {
        warn_defaulted(defaulted, tenant_id, &slug, feature_key, "limit reached");
        return Err(AppError::UpgradeRequired(format!(
            "{} limit reached ({}/{}). Upgrade to increase your limit.",
            label, usage, limit
        )));
    }
    Ok(())
}

/// Enforce a boolean (has_*) feature flag. Unlockable features like calendar,
/// automation, API access. Resolved by `resolve_flag` (feature_limits → features JSONB
/// `has_calendar`/… → dedicated column → refused), which the admin catalogue also calls, so the
/// panel and this gate can never disagree about which tier has the feature.
pub async fn check_feature_flag(
    pool: &PgPool,
    tenant_id: Uuid,
    flag_key: &str,
    label: &str,
) -> Result<(), AppError> {
    // Same rule as the numeric gate above: no active plan → the default plan's flags, which are
    // fail-CLOSED, not "everything granted" (kanban t_f6f94b8e).
    let (slug, defaulted) = resolve_plan_slug(pool, tenant_id).await?;
    match resolve_flag(pool, &slug, flag_key).await?.0 {
        Some(true) => Ok(()),
        // A grant that is present-but-false and a key no plan mentions deny alike: a boolean
        // feature is fail-CLOSED (see feature_registry::FeatureDef::unset_means).
        _ => {
            warn_defaulted(defaulted, tenant_id, &slug, flag_key, "flag not granted");
            Err(AppError::UpgradeRequired(format!(
                "{} is not available on your current plan. Upgrade to access this feature.",
                label
            )))
        }
    }
}

/// Backwards-compat wrapper (4-arg labeled).
pub async fn check_feature_limit(
    pool: &PgPool,
    tenant_id: Uuid,
    feature_key: &str,
    label: &str,
) -> Result<(), AppError> {
    enforce_feature_limit(pool, tenant_id, feature_key, label).await
}

/// Current usage snapshot for the dashboard/me/usage endpoint.
pub async fn get_usage_json(pool: &PgPool, tenant_id: Uuid) -> serde_json::Value {
    let contacts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let phone_numbers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM phone_numbers WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(pool)
            .await
            .unwrap_or(0);
    let rules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM response_rules WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    // `users` has no `is_active` column (see `count_usage`): this query ERRORED on every call and
    // the served usage payload reported 0 team members for every tenant, silently.
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let leads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leads WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let deals: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM deals WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    let workflows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workflows WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(pool)
        .await
        .unwrap_or(0);
    serde_json::json!({
        "contacts": contacts,
        "leads": leads,
        "deals": deals,
        "workflows": workflows,
        "phone_numbers": phone_numbers,
        "rules": rules,
        "users": users
    })
}
