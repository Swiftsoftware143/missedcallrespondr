//! Feature limits enforcement — reads limits from plans table
//! (dedicated columns + `features` JSONB) and enforces per-tenant.
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

/// Fetch a numeric limit for a feature_key.
/// Order of resolution (kanban t_0e61628c — one allowance PER DIMENSION, each overridable per plan):
///   1. `features->>key` — an explicit per-plan OVERRIDE, and it WINS over the plan's own column. The
///      admin writes it from the panel's own "Set plan features (JSON)" action
///      (PUT /api/v1/admin/plans/:id/features, src/handlers/plans_handler.rs), so a CONTACT allowance
///      (or a leads / tags one) can differ from the plan default with no code change and no schema
///      change. CoreSwift-CRM carries its contact allowance exactly this way: a per-tier
///      `features.max_contacts` (100 / 500 / 1 000 / 5 000 / 10 000 / 50 000).
///   2. the dedicated `plans` column for the keys that have one (`max_leads`, `max_tags`) — the
///      plan's DEFAULT, and where every live plan resolves: no `plans` row in this database carries a
///      `max_contacts` or `max_leads` features key (census in the t_0e61628c evidence), so adding the
///      override arm moved no live number.
///   3. `features->>key` for every other key (max_users, max_rules, max_phone_numbers, ...).
///   4. None = no limit declared → allow.
async fn numeric_limit(
    pool: &PgPool,
    slug: &str,
    feature_key: &str,
) -> Result<Option<i64>, AppError> {
    // 1. Dedicated columns. Gate rule 5d (class 14) — a query must not be BUILT at run time, so the
    // whole query is a COMPILE-TIME literal here. The previous
    // `format!("SELECT {} FROM plans WHERE slug = $1", plan_col)` built the query text from a
    // run-time string: the query a request ran was not visible anywhere in this source. Same shape
    // as ADASwift src/features.rs in b2362eb (kanban t_472d6089).
    let column_sql = match feature_key {
        "max_leads" | "leads" | "max_contacts" | "contacts" => {
            Some("SELECT max_leads FROM plans WHERE slug = $1")
        }
        "max_tags" | "tags" => Some("SELECT max_tags FROM plans WHERE slug = $1"),
        _ => None,
    };
    // A key that has a column ALSO has that column as its default, so an explicit features key can
    // only mean anything if it is read FIRST (t_0e61628c).
    if column_sql.is_some() {
        if let Some(v) = jsonb_limit(pool, slug, feature_key).await? {
            return Ok(Some(v));
        }
    }
    if let Some(sql) = column_sql {
        // Dedicated plan columns (max_leads, max_tags) are INT4 (integer).
        if let Some(v) = sqlx::query_scalar::<_, i32>(sql)
            .bind(slug)
            .fetch_optional(pool)
            .await?
        {
            return Ok(Some(v as i64));
        }
    }

    // 3. JSONB features column (covers max_phone_numbers, max_rules, max_users,
    //    max_deals, max_workflows, max_campaigns, max_messages, max_integrations,
    //    max_api_keys, max_follow_ups, max_calls, max_tickets, ...), and a column key whose column
    //    is NULL.
    jsonb_limit(pool, slug, feature_key).await
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
        "max_users" | "users" => {
            Some("SELECT COUNT(*) FROM users WHERE tenant_id = $1 AND is_active = true")
        }
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

/// Enforce a numeric (max_*) feature limit.
pub async fn enforce_feature_limit(
    pool: &PgPool,
    tenant_id: Uuid,
    feature_key: &str,
    label: &str,
) -> Result<(), AppError> {
    let slug = match plan_slug(pool, tenant_id).await? {
        Some(s) => s,
        None => return Ok(()), // no plan → allow
    };
    let limit = match numeric_limit(pool, &slug, feature_key).await? {
        Some(l) => l,
        None => return Ok(()), // no limit configured → allow
    };
    // -1 (or any negative) = unlimited
    if limit < 0 {
        return Ok(());
    }
    // 0 = feature not included on this plan
    if limit == 0 {
        return Err(AppError::UpgradeRequired(format!(
            "{} is not available on your current plan. Upgrade to access this feature.",
            label
        )));
    }
    let usage = count_usage(pool, tenant_id, feature_key).await?;
    if usage >= limit {
        return Err(AppError::UpgradeRequired(format!(
            "{} limit reached ({}/{}). Upgrade to increase your limit.",
            label, usage, limit
        )));
    }
    Ok(())
}

/// Enforce a boolean (has_*) feature flag. Unlockable features like calendar,
/// automation, API access. Reads from features JSONB `has_calendar` etc.
pub async fn check_feature_flag(
    pool: &PgPool,
    tenant_id: Uuid,
    flag_key: &str,
    label: &str,
) -> Result<(), AppError> {
    let slug = match plan_slug(pool, tenant_id).await? {
        Some(s) => s,
        None => return Ok(()), // no plan → allow
    };
    // Boolean from JSONB features: features->>'has_calendar' etc.
    let raw: Option<String> = sqlx::query_scalar("SELECT features->>$1 FROM plans WHERE slug = $2")
        .bind(flag_key)
        .bind(&slug)
        .fetch_optional(pool)
        .await?
        .flatten();
    match raw.as_deref() {
        Some("true") | Some("1") => Ok(()),
        None => {
            // Fall back to dedicated boolean column if it exists (dual-routing etc.). Whole query as
            // a compile-time literal — gate 5d, same reason as numeric_limit above.
            let sql = match flag_key {
                "has_dual_routing" => Some("SELECT has_dual_routing FROM plans WHERE slug = $1"),
                "has_multi_tenant" => Some("SELECT has_multi_tenant FROM plans WHERE slug = $1"),
                "has_white_label" => Some("SELECT has_white_label FROM plans WHERE slug = $1"),
                _ => None,
            };
            if let Some(sql) = sql {
                let v: Option<bool> = sqlx::query_scalar(sql)
                    .bind(&slug)
                    .fetch_optional(pool)
                    .await?
                    .flatten();
                if v == Some(true) {
                    return Ok(());
                }
            }
            Err(AppError::UpgradeRequired(format!(
                "{} is not available on your current plan. Upgrade to access this feature.",
                label
            )))
        }
        _ => Err(AppError::UpgradeRequired(format!(
            "{} is not available on your current plan. Upgrade to access this feature.",
            label
        ))),
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
    let users: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE tenant_id = $1 AND is_active = true")
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
