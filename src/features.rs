//! Feature limits enforcement — reads limits from plans table.
use crate::error::AppError;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn enforce_feature_limit(
    pool: &PgPool,
    tenant_id: Uuid,
    feature_key: &str,
    label: &str,
) -> Result<(), AppError> {
    // Get plan slug from tenant_plans
    let plan_slug: Option<String> = sqlx::query_scalar(
        "SELECT p.slug FROM tenant_plans tp JOIN plans p ON p.id = tp.plan_id WHERE tp.tenant_id = $1 AND tp.status = 'active'"
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await?
    .flatten();

    let slug = match plan_slug {
        Some(s) => s,
        None => return Ok(()),
    };

    // Map feature key to plans table column
    let plan_col = match feature_key {
        "max_leads" | "leads" | "max_contacts" | "contacts" => "max_leads",
        "max_tags" | "tags" => "max_tags",
        _ => return Ok(()), // Unknown — allow
    };

    let limit: Option<i64> =
        sqlx::query_scalar(&format!("SELECT {} FROM plans WHERE slug = $1", plan_col))
            .bind(&slug)
            .fetch_optional(pool)
            .await?
            .flatten();

    match limit {
        None | Some(-1) => Ok(()),
        Some(0) => Err(AppError::UpgradeRequired(format!(
            "{} is not available on your current plan. Upgrade to access this feature.",
            label
        ))),
        Some(limit) => {
            let usage = count_usage(pool, tenant_id, feature_key).await?;
            if usage >= limit {
                Err(AppError::UpgradeRequired(format!(
                    "{} limit reached ({}/{}). Upgrade to increase your limit.",
                    label, usage, limit
                )))
            } else {
                Ok(())
            }
        }
    }
}

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
    serde_json::json!({
        "contacts": contacts,
        "phone_numbers": phone_numbers,
        "rules": rules,
        "users": users
    })
}

async fn count_usage(pool: &PgPool, tenant_id: Uuid, feature_key: &str) -> Result<i64, AppError> {
    match feature_key {
        "max_leads" | "leads" | "max_contacts" | "contacts" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM contacts WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(pool)
        .await?),
        "max_tags" | "tags" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM tags WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(pool)
        .await?),
        "max_phone_numbers" | "phone_numbers" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM phone_numbers WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(pool)
        .await?),
        "max_rules" | "rules" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM response_rules WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(pool)
        .await?),
        "max_users" | "users" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE tenant_id = $1 AND is_active = true",
        )
        .bind(tenant_id)
        .fetch_one(pool)
        .await?),
        _ => Ok(0),
    }
}

/// Backwards-compat wrapper
#[allow(dead_code)]
pub async fn check_feature_limit(
    pool: &PgPool,
    tenant_id: Uuid,
    feature_key: &str,
) -> Result<(), AppError> {
    enforce_feature_limit(pool, tenant_id, feature_key, feature_key).await
}
