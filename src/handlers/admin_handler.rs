use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHasher};
use axum::{
    extract::{Extension, Path, State},
    Json,
};
use rand::rngs::OsRng;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::models::create_token;
use crate::config::Claims;
use crate::error::AppError;
use crate::state::AppState;
use crate::validation::{check_len, max};
use chrono::Utc;

/// Admin sync endpoint called by CoreSwift
pub async fn portfolio_sync(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let sync_id = req
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .unwrap_or_else(Uuid::new_v4);
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Company")
        .to_string();
    let email = req
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Boundary validation for the cross-app admin create path (kanban t_54b1ffab): this route mints
    // a tenant AND a `users` row from a caller-supplied address, so it obeys the same rule the public
    // signup does — before the first SELECT, and the normalised value is what gets stored.
    let email = crate::security::email_addr::normalize(&email).map_err(AppError::Unprocessable)?;

    // The tenant name (and the slug derived from it below) also lands in `portfolio_companies.name`
    // / `.slug`, both VARCHAR(255) — checked before the first statement so a too-long company name is
    // a 400 for the caller instead of a 500 from the driver (kanban t_dd7be032).
    check_len("name", &name, max::PORTFOLIO_COMPANIES_NAME)?;

    // Idempotency on the sync `id` (kanban t_ff373bd1).
    //
    // This door PROVISIONS ACCOUNTS: it mints a `tenants` row plus an argon2-hashed `users` row
    // BEFORE the `portfolio_companies` upsert below. The upsert is keyed on the sync `id`, but the
    // mint was not — so a REPEAT push of a company that already had an account minted a SECOND
    // account (an orphan tenant + generated login the caller was told to "share with the company")
    // that owned no company and was invisible to every tenant-scoped read, and a same-email repeat
    // answered 409 so the sync could not simply be re-run.
    //
    // The account is keyed on the COMPANY, so look the company up FIRST: an id we already hold is a
    // refresh, never a re-provision. Only the descriptive columns move (name, email, description) —
    // NOT `tenant_id` (the scoping key, and the parent of the company's integration/trigger
    // children, which carry their own) and NOT `slug` (bound at mint to its OWN tenant's slug;
    // re-deriving it would manufacture drift). Same reasoning as t_e63b1450's verdict on the
    // upsert's DO UPDATE arm, which this branch mirrors by construction.
    let owner_tenant_id: Option<Uuid> =
        sqlx::query_scalar("SELECT tenant_id FROM portfolio_companies WHERE id = $1")
            .bind(sync_id)
            .fetch_optional(&state.pool)
            .await?;

    if let Some(account_id) = owner_tenant_id {
        sqlx::query(
            "UPDATE portfolio_companies SET name = $2, email = $3, description = $4, updated_at = NOW() WHERE id = $1",
        )
        .bind(sync_id)
        .bind(&name)
        .bind(&email)
        .bind(&description)
        .execute(&state.pool)
        .await?;

        // `already_exists` mirrors the app's own idempotent-provision answer
        // (`provision_handler::already_exists`, documented in docs/ADMIN_GUIDE.md): 200, the
        // account that already owns the company, and NO `user_id`/`password` — nothing was minted
        // this time, so there is no credential to share.
        return Ok(Json(json!({
            "status": "already_exists",
            "id": sync_id.to_string(),
            "name": name,
            "email": email,
            "account_id": account_id.to_string(),
            "note": "This company already has an account; its contact details were refreshed and no new credentials were created."
        })));
    }

    // Check email uniqueness
    let existing =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE lower(email) = $1")
            .bind(&email)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);

    if existing > 0 {
        return Err(AppError::Conflict(format!(
            "A user with email {} already exists",
            email
        )));
    }

    // Create tenant.
    //
    // `tenants.slug` carries UNIQUE `tenants_slug_key`, and this door used to derive the slug from
    // the raw caller-supplied company name (`name.to_lowercase().replace(' ', "_")`) and insert it
    // plainly — so a SECOND `portfolio-sync` push naming a company that already exists answered
    // `500 Database error` straight from the index. Derive it through the app's own account-slug
    // helper instead, the single rule every account door follows (kanban t_1a26f923), and let
    // `ON CONFLICT (slug) DO NOTHING` turn the residual derive/insert race into a retry
    // (kanban t_ff66fbe3). The derived value is `base<=24 + '_' + 8 hex`, far under the limit, and
    // is also what lands in `portfolio_companies.slug`; no response field carries it.
    let tenant_id = uuid::Uuid::new_v4();
    let mut tenant_slug: Option<String> = None;
    for _ in 0..4 {
        let candidate = crate::auth::signup::unique_account_slug(&state.pool, &name).await?;
        let inserted = sqlx::query(
            "INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3) ON CONFLICT (slug) DO NOTHING",
        )
        .bind(tenant_id)
        .bind(&name)
        .bind(&candidate)
        .execute(&state.pool)
        .await?;
        if inserted.rows_affected() == 1 {
            tenant_slug = Some(candidate);
            break;
        }
    }
    let tenant_slug = tenant_slug.ok_or_else(|| {
        AppError::Internal(format!(
            "portfolio_sync: no free tenants.slug for company '{}'",
            name
        ))
    })?;
    check_len("slug", &tenant_slug, max::PORTFOLIO_COMPANIES_SLUG)?;

    // Create user
    let user_id = uuid::Uuid::new_v4();
    let generated_password = Uuid::new_v4()
        .to_string()
        .replace("-", "")
        .chars()
        .take(12)
        .collect::<String>();
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(generated_password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .to_string();

    // Check for duplicate email
    let email_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE lower(email) = $1)")
            .bind(&email)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(false);

    if email_exists {
        return Err(AppError::BadRequest(format!(
            "User with email '{}' already exists",
            email
        )));
    }

    let now = chrono::Utc::now().naive_utc();
    sqlx::query(
        "INSERT INTO users (id, email, password_hash, name, tenant_id, role, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 'company_admin', $6, $7)"
    )
    .bind(user_id)
    .bind(&email)
    .bind(&password_hash)
    .bind(&name)
    .bind(tenant_id)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    // Create portfolio company
    sqlx::query(
        "INSERT INTO portfolio_companies (id, tenant_id, name, slug, email, description, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, NOW(), NOW()) ON CONFLICT (id) DO UPDATE SET name = $3, email = $5, description = $6, updated_at = NOW()"
    )
    .bind(sync_id)
    .bind(tenant_id)
    .bind(&name)
    .bind(&tenant_slug)
    .bind(&email)
    .bind(&description)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({
        "status": "synced",
        "id": sync_id.to_string(),
        "name": name,
        "email": email,
        "account_id": tenant_id.to_string(),
        "user_id": user_id.to_string(),
        "password": generated_password,
        "note": "Share credentials with the company."
    })))
}

/// Admin impersonation
pub async fn impersonate(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Json<Value>, AppError> {
    // ONE definition of "platform admin" in this app (kanban t_92ec2f21): the class gate in
    // `auth_middleware` already fronts this route, and the panel that calls it
    // (admin.missedcallrespondr.com, schema section "4. Tenants, Credits & Impersonation") is
    // handed to a platform admin. The old inline `role != "agency_admin"` was boilerplate from the
    // initial commit f515e9e and no live row carries `agency_admin` (live census: 2 x admin,
    // 23 x account_owner), so it made the panel's own Impersonate action dead for every live user.
    if !crate::auth::middleware::is_platform_admin(&claims.role) {
        return Err(AppError::Unauthorized(
            "Only platform admins can impersonate".into(),
        ));
    }

    let target_tenant_id = req
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("valid account_id is required".into()))?;

    let now = chrono::Utc::now().timestamp() as usize;
    let imp_claims = Claims {
        sub: claims.sub,
        email: format!("impersonated@{}", target_tenant_id),
        aid: target_tenant_id,
        role: "impersonated".to_string(),
        exp: now + 900,
        iat: now,
    };

    let token = create_token(&imp_claims, &state.config.jwt_secret)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(json!({
        "impersonation_token": token,
        "expires_in": 900,
        "token_type": "Bearer",
        "message": "Full account switch. Admin panel disappears."
    })))
}

/// Stop impersonation
pub async fn stop_impersonation() -> Result<Json<Value>, AppError> {
    Ok(Json(json!({
        "status": "impersonation_stopped",
        "note": "Drop impersonation token. Restore admin token."
    })))
}

/// List all tenants (for super admin)
pub async fn list_all_tenants(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, AppError> {
    // Simple auth check — verify token has admin access
    let auth_header = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| AppError::Unauthorized("Missing auth token".into()))?;

    // Validate token
    let claims = crate::auth::models::validate_token(auth_header, &state.config.jwt_secret)
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;

    if claims.role != "agency_admin" && claims.role != "admin" && claims.role != "super_admin" {
        return Err(AppError::Unauthorized(
            "Not authorized to list tenants".into(),
        ));
    }

    use sqlx::Row;
    let rows = sqlx::query(
        r#"SELECT 
            t.id, t.name, t.slug, t.created_at,
            tp.plan_id, tp.status as sub_status, tp.billing_cycle, tp.expires_at,
            tp.credit_balance,
            p.name as plan_name, p.price_monthly, p.price_yearly,
            p.features->>'included_credits' as included_credits,
            (SELECT COUNT(*) FROM users u WHERE u.tenant_id = t.id) as user_count
        FROM tenants t
        LEFT JOIN tenant_plans tp ON tp.tenant_id = t.id
        LEFT JOIN plans p ON p.id = tp.plan_id
        ORDER BY t.created_at DESC"#,
    )
    .fetch_all(&state.pool)
    .await?;

    let tenants: Vec<Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "name": r.try_get::<String,_>("name").unwrap_or_default(),
            "slug": r.try_get::<String,_>("slug").unwrap_or_default(),
            "created_at": r.try_get::<chrono::NaiveDateTime,_>("created_at").map(|d| d.to_string()).unwrap_or_default(),
            "plan_id": r.try_get::<Option<Uuid>,_>("plan_id").ok().flatten().map(|u| u.to_string()),
            "sub_status": r.try_get::<Option<String>,_>("sub_status").ok().flatten(),
            "billing_cycle": r.try_get::<Option<String>,_>("billing_cycle").ok().flatten(),
            "expires_at": r.try_get::<Option<chrono::DateTime<Utc>>,_>("expires_at").ok().flatten().map(|d| d.to_string()),
            "plan_name": r.try_get::<Option<String>,_>("plan_name").ok().flatten(),
            "price_monthly": r.try_get::<Option<f64>,_>("price_monthly").ok().flatten(),
            "price_yearly": r.try_get::<Option<f64>,_>("price_yearly").ok().flatten(),
            "user_count": r.try_get::<Option<i64>,_>("user_count").ok().flatten().unwrap_or(0),
            "credit_balance": r.try_get::<Option<i32>,_>("credit_balance").ok().flatten().unwrap_or(0),
            "included_credits": r.try_get::<Option<String>,_>("included_credits").ok().flatten()
        })
    }).collect();

    Ok(Json(json!({
        "tenants": tenants,
        "total": tenants.len()
    })))
}

/// Admin: add credits to a tenant
pub async fn add_credits(
    State(state): State<AppState>,
    Path(tenant_id_str): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<Value>, AppError> {
    use sqlx::Row;
    let tenant_id = Uuid::parse_str(&tenant_id_str)
        .map_err(|_| AppError::BadRequest("Invalid tenant ID".into()))?;
    let amount = body
        .get("amount")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| AppError::BadRequest("amount (integer) required".into()))?
        as i32;

    if amount <= 0 {
        return Err(AppError::BadRequest("Amount must be positive".into()));
    }

    // Upsert tenant_plan with credit increase
    let existing = sqlx::query("SELECT credit_balance FROM tenant_plans WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_optional(&state.pool)
        .await?;

    match existing {
        Some(row) => {
            let current: i32 = row.try_get("credit_balance").unwrap_or(0);
            let new_balance = current.checked_add(amount).unwrap_or(i32::MAX);
            sqlx::query(
                "UPDATE tenant_plans SET credit_balance = $1, lifetime_credits = lifetime_credits + $2, updated_at = NOW() WHERE tenant_id = $3"
            )
            .bind(new_balance)
            .bind(amount)
            .bind(tenant_id)
            .execute(&state.pool)
            .await?;
        }
        None => {
            // Try to find any plan to associate, or use a dummy fallback
            let plan = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM plans ORDER BY sort_order ASC LIMIT 1",
            )
            .fetch_optional(&state.pool)
            .await?;
            if let Some(pid) = plan {
                sqlx::query(
                    "INSERT INTO tenant_plans (tenant_id, plan_id, credit_balance, lifetime_credits, status, billing_cycle) VALUES ($1, $2, $3, $4, 'active', 'manual')"
                )
                .bind(tenant_id)
                .bind(pid)
                .bind(amount)
                .bind(amount)
                .execute(&state.pool)
                .await?;
            } else {
                return Err(AppError::Internal(
                    "No plans exist. Create a plan first.".into(),
                ));
            }
        }
    }

    Ok(Json(json!({
        "message": format!("Added {} credits", amount),
        "tenant_id": tenant_id_str
    })))
}

/// David's sister companies (portfolio). Their accounts exist in every app and are kept by rule, so a
/// delete must never be able to remove one.
///
/// NOTE (measured live 2026-10-10): this app's `tenants` table has NO `is_portfolio` column — the
/// markers below are the whole test here. Where the column DOES exist (CoreSwift-CRM, incentive
/// siblings) it is checked first and a real `true` protects the row.
const PORTFOLIO_TENANT_MARKERS: [&str; 3] = ["swiftimpact", "zaarhub", "giraudy"];

/// Refuse to delete two things, on the SINGLE route and the BULK route alike — one helper, so the two
/// doors can never drift apart (kanban t_31951eca).
///
/// Refused: the workspace the caller is SIGNED IN AS (`claims.aid`) — that is a lockout, not a
/// cleanup. Refused: a portfolio company, matched on the workspace's name/slug (`tenants.is_portfolio`
/// where the column exists, plus the marker words).
///
/// Everything else behaves as a plain `DELETE FROM tenants`, which retires the whole workspace: every
/// other foreign key to `tenants` is ON DELETE CASCADE, and migration 000035 arms the one edge that
/// was not (`provider_keys`), so no hand-rolled child sweep is needed.
async fn guard_protected(
    state: &AppState,
    tenant_id: Uuid,
    caller_tenant_id: Uuid,
) -> Result<(), AppError> {
    if tenant_id == caller_tenant_id {
        return Err(AppError::BadRequest(
            "refusing to delete the workspace you are signed in as".into(),
        ));
    }

    let row: Option<(String, String)> =
        sqlx::query_as("SELECT COALESCE(name, ''), COALESCE(slug, '') FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .fetch_optional(&state.pool)
            .await?;

    if let Some((name, slug)) = row {
        let hay = format!("{} {}", name, slug).to_lowercase();
        if let Some(hit) = PORTFOLIO_TENANT_MARKERS.iter().find(|m| hay.contains(*m)) {
            return Err(AppError::BadRequest(format!(
                "refusing to delete a portfolio company ({})",
                hit
            )));
        }
    }

    Ok(())
}

/// The refusal's own sentence. `AppError` carries the message but implements no `Display`, and the
/// bulk answer must report the REASON next to the id it kept, so unwrap the variants here.
fn refusal_message(e: &AppError) -> String {
    match e {
        AppError::BadRequest(m)
        | AppError::Unauthorized(m)
        | AppError::NotFound(m)
        | AppError::Internal(m)
        | AppError::Conflict(m)
        | AppError::UpgradeRequired(m)
        | AppError::Unprocessable(m)
        | AppError::ServiceUnavailable(m)
        | AppError::UpstreamRefused(m) => m.clone(),
    }
}

/// Admin: delete a tenant and all associated data.
///
/// Guarded by [`guard_protected`] (kanban t_31951eca): until that card this route deleted ANY id it
/// was handed, including the workspace the operator was signed in as.
pub async fn delete_tenant(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(tenant_id_str): Path<String>,
) -> Result<Json<Value>, AppError> {
    let tenant_id = Uuid::parse_str(&tenant_id_str)
        .map_err(|_| AppError::BadRequest("Invalid tenant ID".into()))?;

    guard_protected(&state, tenant_id, claims.aid).await?;

    let result = sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Tenant not found".into()));
    }

    Ok(Json(json!({
        "message": "Tenant deleted",
        "tenant_id": tenant_id_str
    })))
}

/// POST /api/v1/admin/tenants/bulk-delete — retire several workspaces in one call (kanban t_31951eca,
/// the missedcallrespondr leg of t_ac2fe688).
///
/// The counterpart of the same control in the CoreSwift-CRM, IncentiveSwift and FunnelSwift panels, so
/// an operator no longer has to open a psql session to clear probe/junk workspaces out of the
/// console's own tenant list. Answers PER ID, so one refused or missing id cannot sink the batch — the
/// panel shows the reason next to the row that was kept. Both refusals come from the SAME
/// [`guard_protected`] the single route uses.
pub async fn bulk_delete_tenants(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let raw_ids = req
        .get("ids")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut deleted_ids: Vec<String> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();

    for v in raw_ids {
        let raw = v.as_str().unwrap_or_default().trim().to_string();

        let tenant_id = match Uuid::parse_str(&raw) {
            Ok(t) => t,
            Err(_) => {
                failed.push(json!({"id": raw, "error": "not a valid id"}));
                continue;
            }
        };

        if let Err(e) = guard_protected(&state, tenant_id, claims.aid).await {
            failed.push(json!({"id": raw, "error": refusal_message(&e)}));
            continue;
        }

        match sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .execute(&state.pool)
            .await
        {
            Ok(r) if r.rows_affected() == 0 => {
                failed.push(json!({"id": raw, "error": "tenant not found"}))
            }
            Ok(_) => deleted_ids.push(raw.clone()),
            Err(e) => failed.push(json!({"id": raw, "error": e.to_string()})),
        }
    }

    Ok(Json(json!({
        "status": "ok",
        "deleted": deleted_ids.len(),
        "deleted_ids": deleted_ids,
        "failed": failed
    })))
}
