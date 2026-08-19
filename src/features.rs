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
/// Order of resolution:
///   1. dedicated `plans` column if the key maps to one,
///   2. `features` JSONB value (features->>key)::bigint,
///   3. None (no limit → allow).
async fn numeric_limit(
    pool: &PgPool,
    slug: &str,
    feature_key: &str,
) -> Result<Option<i64>, AppError> {
    // 1. Dedicated columns
    let plan_col = match feature_key {
        "max_leads" | "leads" | "max_contacts" | "contacts" => "max_leads",
        "max_tags" | "tags" => "max_tags",
        _ => "",
    };
    if !plan_col.is_empty() {
        // Dedicated plan columns (max_leads, max_tags) are INT4 (integer).
        if let Some(v) =
            sqlx::query_scalar::<_, i32>(&format!("SELECT {} FROM plans WHERE slug = $1", plan_col))
                .bind(slug)
                .fetch_optional(pool)
                .await?
        {
            return Ok(Some(v as i64));
        }
    }

    // 2. JSONB features column (covers max_phone_numbers, max_rules, max_users,
    //    max_deals, max_workflows, max_campaigns, max_messages, max_integrations,
    //    max_api_keys, max_follow_ups, max_calls, max_tickets, ...)
    let v: Option<i64> =
        sqlx::query_scalar("SELECT (features->>$1)::bigint FROM plans WHERE slug = $2")
            .bind(feature_key)
            .bind(slug)
            .fetch_optional(pool)
            .await?
            .flatten();
    Ok(v)
}

/// Count current usage for a feature key (per tenant).
async fn count_usage(pool: &PgPool, tenant_id: Uuid, key: &str) -> Result<i64, AppError> {
    let q = match key {
        "max_contacts" | "contacts" | "max_leads" | "leads" => {
            Some("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1")
        }
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
            // Fall back to dedicated boolean column if it exists (dual-routing etc.)
            let col = match flag_key {
                "has_dual_routing" => "has_dual_routing",
                "has_multi_tenant" => "has_multi_tenant",
                "has_white_label" => "has_white_label",
                _ => "",
            };
            if !col.is_empty() {
                let v: Option<bool> =
                    sqlx::query_scalar(&format!("SELECT {} FROM plans WHERE slug = $1", col))
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
