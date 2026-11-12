//! Plan resolution for the private-inbox module (the MissedCall Respondr half of CoreSwift's
//! `module_registry` seam).
//!
//! The inbox was ported from CoreSwift, where a `module_registry` table answers "does this tenant's
//! plan include private email, and what are its numeric limits". MissedCall Respondr has no module registry —
//! its catalogue is `plans.features` (JSONB) joined through `tenant_plans` — so this module answers
//! the SAME questions from the data this app already sells:
//!
//! * `private_email` module: granted when the tenant is seated on an ACTIVE plan. A tenant with no
//!   plan row is `no_plan` and is refused exactly as CoreSwift refuses it (an inbox is a sold
//!   feature; "no plan" is not a plan).
//! * numeric limits (`email_domains`, `email_mailboxes`): read `plans.features->>'max_domains'` /
//!   `->>'max_mailboxes'`. A plan that states no number is UNLIMITED — the ceiling exists only
//!   where it was sold, and inventing a default here would refuse a customer for a limit nobody
//!   sold them. `tenant_email_limits` overrides still win, because that table is the operator's own
//!   per-account tool and it is consulted before the plan.
//!
//! Deliberately small: three call sites in `private_email::feature_gate`, one query each, no cache
//! to go stale when an operator moves a workspace between plans.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;

/// What one feature resolves to for a tenant.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// May this tenant use the feature at all?
    pub enabled: bool,
    /// Where the answer came from: `plan`, `override`, or `no_plan`. Callers treat `no_plan` as
    /// "refused" even when `enabled` is tolerant, which is what keeps a plan-less account from
    /// receiving unlimited domains.
    pub source: &'static str,
    /// The numeric ceiling, when the feature is a limit. `None` = no ceiling stated.
    #[allow(dead_code)]
    // Ported with the inbox (fleet-identical surface); no screen on this app reaches it yet.
    pub limit_value: Option<f64>,
}

impl Resolved {
    fn refused(source: &'static str) -> Self {
        Self {
            enabled: false,
            source,
            limit_value: None,
        }
    }
}

/// The tenant's most specific active plan row (highest `sort_order`, matching the app's own
/// "best plan wins" rule used by the billing surface), or `None`.
async fn active_plan_features(pool: &PgPool, tenant_id: Uuid) -> Result<Option<Value>, AppError> {
    let features: Option<Option<Value>> = sqlx::query_scalar(
        "SELECT p.features
           FROM tenant_plans tp
           JOIN plans p ON p.id = tp.plan_id
          WHERE tp.tenant_id = $1 AND p.is_active
          ORDER BY p.id
          LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;
    Ok(features.flatten())
}

/// A number spelled in the plan blob, tolerant of numbers-as-strings (the admin editor stores both).
fn number_at(features: &Value, key: &str) -> Option<f64> {
    match features.get(key) {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Does the plan blob say the feature is granted? (`true`, or a non-zero numeric cap.)
fn granted_at(features: &Value, key: &str) -> bool {
    match features.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|v| v > 0.0).unwrap_or(false),
        Some(Value::String(s)) => matches!(s.trim(), "true" | "1" | "yes"),
        _ => false,
    }
}

/// Resolve one feature for one tenant — the ported module's `resolve(pool, tenant, key)`.
pub async fn resolve(pool: &PgPool, tenant_id: Uuid, key: &str) -> Result<Resolved, AppError> {
    // An operator's own per-account override is the most specific answer and always wins.
    let overrides: Option<(Option<i32>, Option<i32>, Option<i32>)> = sqlx::query_as(
        "SELECT max_domains, max_mailboxes, max_aliases_per_mailbox
           FROM tenant_email_limits WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;

    let override_limit = overrides.and_then(|(domains, boxes, _aliases)| match key {
        "email_domains" => domains,
        "email_mailboxes" => boxes,
        _ => None,
    });
    if let Some(limit) = override_limit {
        return Ok(Resolved {
            enabled: limit != 0,
            source: "override",
            limit_value: Some(limit as f64),
        });
    }

    let Some(features) = active_plan_features(pool, tenant_id).await? else {
        return Ok(Resolved::refused("no_plan"));
    };

    match key {
        "private_email" => Ok(Resolved {
            enabled: true,
            source: "plan",
            limit_value: None,
        }),
        "email_domains" => Ok(Resolved {
            enabled: true,
            source: "plan",
            limit_value: number_at(&features, "max_domains"),
        }),
        "email_mailboxes" => Ok(Resolved {
            enabled: true,
            source: "plan",
            limit_value: number_at(&features, "max_mailboxes"),
        }),
        other => Ok(Resolved {
            enabled: granted_at(&features, other),
            source: "plan",
            limit_value: number_at(&features, other),
        }),
    }
}
