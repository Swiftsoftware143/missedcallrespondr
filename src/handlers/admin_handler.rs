use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHasher};
use axum::{
    extract::{Extension, Path, Query, State},
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
        // NOT marked with `probe_harness` on purpose (kanban t_4a744754): an AUTHENTICATED
        // admin-console door, not the served, unauthenticated signup the marker exists for.
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

    // A portfolio tenant is one of David's own companies, so it always gets this app's HIGHEST level
    // (his rule, 2026-10-04). This door used to mint the workspace with no `tenant_plans` row at all,
    // so the sister companies the fleet pushes here landed on the free defaults — measured live
    // 2026-10-17 on the app's own "Giraudy Capital" tenant, which had no tier. See
    // `plans_handler::seat_top_plan_on` (best-effort: a provision is never failed over a catalogue).
    crate::handlers::plans_handler::seat_top_plan_on(&state, tenant_id).await;

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
        // An impersonation session is not a signup door: it may never claim the minted-password
        // privilege that `set_initial_password` accepts.
        pwd_minted: false,
        exp: now + 900,
        iat: now,
        wsid: None,
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

/// Is this workspace OWNER's address fleet machinery rather than a customer?
///
/// The two arms are the fleet's own definitions, not a guess at intent:
///  * `security::probe_addr::attribution_domain` — the harness domains the mail layer suppresses
///    (`swiftsoftware.dev/.net/.local`, `example.*`, `.invalid`, `.test`) plus the fleet's
///    disposable inbox (`maxxspace.com`). A workspace minted on one is a probe BY CONSTRUCTION.
///  * `security::email_addr::is_reserved_address` — the RFC 2606/6761 class that can never receive
///    mail, so it cannot be a customer's login.
///
/// An ordinary address on a routable domain answers `false`, so a real customer row is untouched.
fn owner_is_fleet_machinery(email: &str) -> bool {
    let addr = email.trim();
    if addr.is_empty() {
        return false;
    }
    crate::security::probe_addr::attribution_domain(addr).is_some()
        || crate::security::email_addr::is_reserved_address(addr)
}

/// List all tenants (for super admin)
pub async fn list_all_tenants(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
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
    // `owner_email` (added 2026-10-15, kanban t_81a1ba13) is the WORKSPACE'S OWNER as the operator's
    // list has to show it. `tenants` carries no email column at all here, so before this the row
    // (name, plan, status) was the whole answer and an operator could not tell whose workspace it
    // was — a leftover probe workspace read exactly like a customer. The identity lives in `users`:
    // the workspace's own owner-ish role first (`register` mints `account_owner`), else the earliest
    // member. NULL only when the workspace has no user row at all (e.g. a portfolio-sync row).
    // Did the operator ask for the probe workspaces too (`?include_probe=1`)? Hoisted out of the
    // bind so the same answer decides BOTH the SQL marker arm and the address arm below.
    let include_probe = params
        .get("include_probe")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let rows = sqlx::query(
        r#"SELECT 
            t.id, t.name, t.slug, t.created_at, COALESCE(t.probe_harness, '') AS probe_harness,
            -- Whether this workspace is OPEN (2026-11-01). `auth::handlers::login` and
            -- `auth::middleware` now refuse a closed one, so the console must be able to show the
            -- state it is setting -- otherwise the operator suspends a row and the list says
            -- nothing about it.
            t.is_active AS tenant_active,
            tp.plan_id, tp.status as sub_status, tp.billing_cycle, tp.expires_at,
            tp.credit_balance,
            p.name as plan_name, p.slug as plan_slug, p.price_monthly::float8 AS price_monthly,
            -- The plan the GATES actually enforce for this workspace. `features::resolve_plan_slug`
            -- falls back to "free" when the tenant has no active `tenant_plans` row, but the list
            -- above published NULL for those rows, so the operator saw "— no plan —" where the app
            -- was enforcing a real tier. features.rs states the rule this restores: what the panel
            -- shows and what the gate read must not diverge.
            COALESCE(p.name, (SELECT dp.name FROM plans dp
                              WHERE dp.slug = 'free' AND dp.is_active
                              ORDER BY dp.sort_order NULLS LAST, dp.created_at LIMIT 1)) AS plan_effective_name,
            p.price_yearly::float8 AS price_yearly,
            p.features->>'included_credits' as included_credits,
            (SELECT COUNT(*) FROM users u WHERE u.tenant_id = t.id) as user_count,
            -- The workspace's newest successful sign-in (migration 000046). NULL is rendered by the
            -- console as "not recorded" rather than "never": the column starts empty and earlier
            -- sign-ins were not tracked, so "never" would be a claim about a customer this table
            -- cannot make. Either way the cell is never blank and never a zero date.
            (SELECT max(u.last_login_at) FROM users u WHERE u.tenant_id = t.id) as last_sign_in,
            (SELECT u.email FROM users u WHERE u.tenant_id = t.id
             ORDER BY CASE WHEN u.role IN ('account_owner','company_admin','admin','super_admin','agency_admin')
                           THEN 0 ELSE 1 END,
                      u.created_at ASC
             LIMIT 1) as owner_email,
             -- DID THE OWNER EVER RECEIVE THE PASSWORD WE MINTED? (cron 2026-11-05)
             --
             -- `users.password_undelivered` is set by the signup door when it mints a first password
             -- and the credentials mail does not go out. The app has always KNOWN that, per user, and
             -- no surface published it: the operator could only learn it from an Email Log refusal or
             -- from the customer complaining they cannot sign in. This column puts the state on the
             -- row it belongs to, beside the address, which is what lets the Owner cell badge it and
             -- offer the re-send action (`resend_owner_credentials`).
             --
             -- The SAME owner ordering as `owner_email` above, so the badge and the address always
             -- describe one person. A workspace with no user reads FALSE (nothing to deliver).
             COALESCE((SELECT u.password_undelivered FROM users u WHERE u.tenant_id = t.id
              ORDER BY CASE WHEN u.role IN ('account_owner','company_admin','admin','super_admin','agency_admin')
                            THEN 0 ELSE 1 END,
                       u.created_at ASC
              LIMIT 1), false) as owner_credentials_undelivered
        FROM tenants t
        LEFT JOIN tenant_plans tp ON tp.tenant_id = t.id AND tp.status = 'active'
        LEFT JOIN plans p ON p.id = tp.plan_id
        WHERE $1::bool
           OR COALESCE(t.probe_harness, '') = ''
           OR t.probe_harness ~* '^CNRY'
        ORDER BY t.created_at DESC"#,
    )
    .bind(include_probe)
    .fetch_all(&state.pool)
    .await?;

    // A HARNESS THAT NEVER SENT THE HEADER IS STILL NOT A CUSTOMER (measured live 2026-10-31).
    // `tenants.probe_harness` is stamped ONLY from the `X-Swift-Harness` request header (or the
    // signup door's server-derived domain arm, which post-dates these rows), so an address-only
    // probe row had NO marker: this install's operator list served "Tenancy Canary A/B"
    // (`probe-canary-a/b@swiftsoftware.dev`) as customers — 6 rows, 2 of them machinery — while
    // the served console already badged them "a reserved or fleet-probe address - a probe, not a
    // customer" and its own comment says the server holds probe rows back. The two surfaces
    // disagreed about the same row.
    //
    // The OWNER ADDRESS is the second signal every workspace has: an address on a reserved /
    // fleet-harness domain (see `owner_is_fleet_machinery`) cannot receive mail and cannot belong
    // to a customer. Such a row is now held back exactly like a tagged harness row and published
    // with `reserved_owner: true` when `?include_probe=1` asks for it. Fleet canaries keep the
    // `^CNRY` exemption in SQL.
    //
    // A WORKSPACE NOBODY CAN SIGN INTO IS NOT A CUSTOMER (measured live 2026-11-01). The address
    // arm above can only judge a row that HAS an owner, so the one workspace on this install with
    // no user row at all -- "TestSync", user_count 0 -- was served among three real customers in
    // the operator's Tenants list. ADASwift closed the same hole on 2026-10-31 using the count this
    // query already returns: a customer workspace always has at least the user its signup minted,
    // so zero users means no login can ever reach it and no signup can be attributed to it. It is
    // a leftover, not a customer. Held-back rows are published with `ownerless: true` under
    // `?include_probe=1`, so nothing becomes invisible or undeletable and the console's
    // "Select probes" control can still reach them.
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| {
            include_probe
                || !is_hidden_workspace(
                    &r.try_get::<String, _>("probe_harness").unwrap_or_default(),
                    r.try_get::<Option<String>, _>("owner_email")
                        .ok()
                        .flatten()
                        .as_deref(),
                    r.try_get::<Option<i64>, _>("user_count")
                        .ok()
                        .flatten()
                        .unwrap_or(0),
                )
        })
        .collect();

    let tenants: Vec<Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "name": r.try_get::<String,_>("name").unwrap_or_default(),
            "slug": r.try_get::<String,_>("slug").unwrap_or_default(),
            "created_at": r.try_get::<chrono::NaiveDateTime,_>("created_at").map(|d| d.to_string()).unwrap_or_default(),
            // TRUE => the operator has shut this workspace: sign-in answers 403 and every live
            // session is refused (2026-11-01, cron `default`). A missing column reads as OPEN
            // (unwrap_or(true)) so a query that predates it can never render a customer as
            // suspended by accident.
            "suspended": !r.try_get::<Option<bool>,_>("tenant_active").ok().flatten().unwrap_or(true),
            "plan_id": r.try_get::<Option<Uuid>,_>("plan_id").ok().flatten().map(|u| u.to_string()),
            "sub_status": r.try_get::<Option<String>,_>("sub_status").ok().flatten(),
            "billing_cycle": r.try_get::<Option<String>,_>("billing_cycle").ok().flatten(),
            "expires_at": r.try_get::<Option<chrono::DateTime<Utc>>,_>("expires_at").ok().flatten().map(|d| d.to_string()),
            "plan_name": r.try_get::<Option<String>,_>("plan_name").ok().flatten(),
            // The plan's SLUG alongside its name (2026-10-19): the Tenants screen's plan cell is a
            // picker now, so it needs the stable key the assign writer takes, not just a label.
            "plan_slug": r.try_get::<Option<String>,_>("plan_slug").ok().flatten(),
            // The tier the gates enforce when no plan row is assigned (see the query note above),
            // and whether this row is running on that default rather than an explicit assignment.
            "plan_effective_name": r.try_get::<Option<String>,_>("plan_effective_name").ok().flatten(),
            "plan_is_default": r.try_get::<Option<Uuid>,_>("plan_id").ok().flatten().is_none(),
            "price_monthly": r.try_get::<Option<f64>,_>("price_monthly").ok().flatten(),
            "price_yearly": r.try_get::<Option<f64>,_>("price_yearly").ok().flatten(),
            "user_count": r.try_get::<Option<i64>,_>("user_count").ok().flatten().unwrap_or(0),
            // true => this workspace has NO user at all, so nobody can sign in and no signup can
            // be attributed to it: a leftover, not a customer. Held back from the default list
            // (see the filter above) and only reachable with `?include_probe=1`, mirroring
            // ADASwift's `ownerless` field, so the console can badge the row it reveals.
            "ownerless": r.try_get::<Option<i64>,_>("user_count").ok().flatten().unwrap_or(0) <= 0,
            "last_sign_in": r.try_get::<Option<chrono::DateTime<Utc>>,_>("last_sign_in").ok().flatten().map(|d| d.to_string()),
            "owner_email": r.try_get::<Option<String>,_>("owner_email").ok().flatten(),
            // TRUE => the signup door minted this workspace's first password and the mail carrying
            // it never went out, so nobody has ever read that customer's password (2026-11-05).
            // Published so the Owner cell can badge the row and offer `resend-credentials`; a
            // missing column reads as FALSE so an older query can never badge a healthy customer.
            "owner_credentials_undelivered": r
                .try_get::<Option<bool>, _>("owner_credentials_undelivered")
                .ok()
                .flatten()
                .unwrap_or(false),
            // The harness marker the signup door stamps from the `X-Swift-Harness` HEADER
            // (`auth::signup::create_account`). It was written since 2026-10-15 and read by
            // nothing: the operator list could only GUESS a probe from a reserved-domain owner
            // address, so a harness workspace whose user row is gone (owner_email NULL) read
            // exactly like a broken customer workspace. NULL for every real customer signup.
            "probe_harness": r.try_get::<Option<String>,_>("probe_harness").ok().flatten(),
            // true => the owner's address is on a reserved / fleet-harness domain, so this
            // workspace is fleet machinery rather than a customer. Only reachable with
            // `?include_probe=1`, mirroring FunnelSwift's `reserved_owner`.
            "reserved_owner": r
                .try_get::<Option<String>, _>("owner_email")
                .ok()
                .flatten()
                .map(|e| owner_is_fleet_machinery(&e))
                .unwrap_or(false),
            "credit_balance": r.try_get::<Option<i32>,_>("credit_balance").ok().flatten().unwrap_or(0),
            "included_credits": r.try_get::<Option<String>,_>("included_credits").ok().flatten()
        })
    }).collect();

    Ok(Json(json!({
        "tenants": tenants,
        "total": tenants.len()
    })))
}

/// POST /api/v1/admin/tenants/:id/resend-credentials
///
/// THE OPERATOR'S REMEDY FOR "I never got the email and I can't sign in" (cron 2026-11-05).
///
/// `users.password_undelivered` has recorded that state since the signup door started minting
/// first passwords, and `list_all_tenants` now publishes it on the row
/// (`owner_credentials_undelivered`) so the console can badge it. This is the action behind the
/// badge: without it the operator could SEE a stuck customer and still had no way to help them.
/// The only other doors that clear the latch are the CUSTOMER's own — the one-shot
/// `initial-password` choice, which needs the session signup returned, or a reset code that
/// travels the same mail path that just failed.
///
/// It mints a NEW password with the app's own generator, stores only its hash, clears the latch,
/// ends the account's existing sessions (a re-issued password means the old one is no longer a
/// credential, so a token minted from it must not outlive it) and mails it through the SAME
/// `welcome_credentials` template the signup door uses.
///
/// `handover_password` is returned ONLY when nothing left — a reserved/harness address (withheld
/// at the sender) or a provider refusal — because otherwise the operator would have nothing to
/// hand the customer. The password itself is never logged.
///
/// SINCE 2026-11-14 it also REPAIRS a workspace with no login at all: when the workspace holds not
/// one `users` row, the request's optional `{ "email": … }` names its first owner, that row is
/// minted here (role `account_owner`, latch set) and the same re-issue and mail run for it. Before
/// this the route answered `404 "no user to send a password to"` on a workspace that was otherwise
/// complete — the one state where the operator's only repair was SQL — and the console's badge for
/// it points at exactly this action.
pub async fn resend_owner_credentials(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(tenant_id_str): Path<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, AppError> {
    // The same admin gate `list_all_tenants` applies. This route also sits under
    // `/api/v1/admin/`, so the middleware covers it; the handler refuses on its own as well so the
    // ACTION's authority never depends on a surface rule that may change.
    let auth_header = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| AppError::Unauthorized("Missing auth token".into()))?;
    let claims = crate::auth::models::validate_token(auth_header, &state.config.jwt_secret)
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;
    if claims.role != "agency_admin" && claims.role != "admin" && claims.role != "super_admin" {
        return Err(AppError::Forbidden(
            "Not authorized to reissue credentials".into(),
        ));
    }

    use sqlx::Row;
    let tenant_id = Uuid::parse_str(&tenant_id_str)
        .map_err(|_| AppError::BadRequest("Invalid tenant ID".into()))?;

    // The SAME owner ordering the list uses, so the badge and this action always describe one
    // person: the workspace's owner-ish role first, else its earliest member.
    let row = sqlx::query(
        r#"SELECT u.id, u.email, COALESCE(NULLIF(u.name, ''), u.email) AS name
             FROM users u
            WHERE u.tenant_id = $1
            ORDER BY CASE WHEN u.role IN ('account_owner','company_admin','admin','super_admin','agency_admin')
                          THEN 0 ELSE 1 END,
                     u.created_at ASC
            LIMIT 1"#,
    )
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;

    // A workspace that never got its FIRST login is the same stuck customer this action exists for
    // (measured live 2026-11-14: 'Xride Drivers' was active with 0 users, so the console's "Send
    // login" and the People panel had nothing to act on and the only repair was SQL). Mint the
    // first owner here instead of answering a dead end. The workspace row carries no email of its
    // own, so the address comes from the request's optional body; everything after this point —
    // generated password, `welcome_credentials` mail, the handover when nothing left — is the
    // SAME path an existing owner takes.
    let (user_id, email, name) = match row {
        Some(r) => (
            r.try_get("id")
                .map_err(|e| AppError::Internal(e.to_string()))?,
            r.try_get("email").unwrap_or_default(),
            r.try_get("name").unwrap_or_else(|_| String::new()),
        ),
        None => {
            let req = body.map(|Json(v)| v).unwrap_or(Value::Null);
            let asked = req
                .get("email")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AppError::Unprocessable(
                        "This workspace has no login yet. Give it the owner's email address — \
                         nobody can sign in without one."
                            .into(),
                    )
                })?;
            let parts: Vec<&str> = asked.split('@').collect();
            let shaped = parts.len() == 2
                && !parts[0].is_empty()
                && parts[1].contains('.')
                && !parts[1].starts_with('.')
                && !asked.chars().any(char::is_whitespace);
            if !shaped {
                return Err(AppError::Unprocessable(
                    "email: that value is not a valid email address.".into(),
                ));
            }
            let taken: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE lower(email) = $1")
                    .bind(&asked)
                    .fetch_one(&state.pool)
                    .await?;
            if taken > 0 {
                return Err(AppError::Conflict(format!(
                    "An account already exists for '{asked}', so it cannot own a second workspace."
                )));
            }
            let tenant_name: String = sqlx::query_scalar("SELECT name FROM tenants WHERE id = $1")
                .bind(tenant_id)
                .fetch_optional(&state.pool)
                .await?
                .unwrap_or_default();
            let name = req
                .get("name")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    let n = tenant_name.trim();
                    if n.is_empty() {
                        asked.split('@').next().unwrap_or("Owner").to_string()
                    } else {
                        n.to_string()
                    }
                });
            // A throwaway hash: the re-issue below mints the password that is actually mailed and
            // overwrites this value, so there is exactly ONE credential in play.
            let placeholder =
                crate::auth::models::hash_password(&crate::auth::signup::generate_temp_password())
                    .map_err(|e| AppError::Internal(e.to_string()))?;
            let uid = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO users (id, email, password_hash, name, tenant_id, role, \
                 password_undelivered, created_at, updated_at) \
                 VALUES ($1, $2, $3, $4, $5, 'account_owner', TRUE, NOW(), NOW())",
            )
            .bind(uid)
            .bind(&asked)
            .bind(&placeholder)
            .bind(&name)
            .bind(tenant_id)
            .execute(&state.pool)
            .await
            .map_err(|e| {
                if let sqlx::Error::Database(db) = &e {
                    if db.constraint() == Some("users_email_key") {
                        return AppError::Conflict(format!(
                            "An account already exists for '{asked}'."
                        ));
                    }
                }
                AppError::from(e)
            })?;
            tracing::info!(
                tenant = %tenant_id,
                user = %uid,
                "operator created the FIRST login for a workspace that had none"
            );
            (uid, asked, name)
        }
    };

    let password = crate::auth::signup::generate_temp_password();
    let password_hash = crate::auth::models::hash_password(&password)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    sqlx::query(
        "UPDATE users SET password_hash = $1, password_undelivered = false, \
                sessions_valid_from = NOW(), updated_at = NOW() \
           WHERE id = $2",
    )
    .bind(&password_hash)
    .bind(user_id)
    .execute(&state.pool)
    .await?;

    // A reserved/harness address is WITHHELD at the sender (it answers Ok without queueing
    // anything), so it counts as NOT delivered — otherwise the console would tell the operator to
    // wait for a mail that was deliberately never sent. Same predicate `auth::register` uses.
    let withheld = crate::security::probe_addr::harness_domain(&email).is_some();
    let mut emailed = false;
    if !withheld {
        let vars = json!({
            "name": name,
            "email": email,
            "password": password,
        });
        match crate::email::send_template_email(
            &state.pool,
            tenant_id,
            &email,
            "welcome_credentials",
            &vars,
        )
        .await
        {
            Ok(()) => emailed = true,
            Err(e) => {
                tracing::error!(
                    tenant = %tenant_id,
                    user = %user_id,
                    "operator re-issued a password but the mail failed: {}",
                    e
                );
            }
        }
    }

    tracing::info!(
        tenant = %tenant_id,
        user = %user_id,
        emailed,
        withheld,
        "operator re-issued a workspace owner's password"
    );

    Ok(Json(json!({
        "id": user_id.to_string(),
        "email": email,
        "credentials_emailed": emailed,
        // Published so the console can say WHY it is handing over a password instead of claiming
        // "check your email" for a message that was deliberately never sent.
        "withheld": withheld,
        "handover_password": if emailed { Value::Null } else { json!(password) },
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
            // Try to find any plan to associate, or use a dummy fallback.
            // Deterministic (kanban t_b73c597e): `sort_order` defaults to 0 and only the seeded rows
            // set it, so Enterprise and Pro both sit at 0 and this `LIMIT 1` used to pick whichever
            // row the planner happened to return first — the same inconsistent-sort_order class as
            // the Plans list. The tie now breaks on price, then name, so a credit grant can never
            // seat the $199 tier by accident.
            //
            // ACTIVE ONLY, cheapest first (measured live 2026-10-17): the operator RETIRED "Pro"
            // (is_active=false, sort_order 0, $49) when "Pro Monthly" replaced it, and the old
            // ordering still picked the retired row for a credit grant — so a tenant with no plan
            // silently received 100 leads / 50 tags / dual routing, i.e. a paid tier's entitlements,
            // from a grant that only ever promises credits. A grant is not a purchase: seat the
            // cheapest plan the operator still offers (the free tier, when there is one) and never
            // one they have switched off.
            let plan = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM plans WHERE is_active = true \
                 ORDER BY price_monthly ASC NULLS LAST, sort_order ASC, name ASC LIMIT 1",
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
        | AppError::Forbidden(m)
        | AppError::NotFound(m)
        | AppError::Internal(m)
        | AppError::Conflict(m)
        | AppError::UpgradeRequired(m)
        | AppError::PayloadTooLarge(m)
        | AppError::Unprocessable(m)
        | AppError::ServiceUnavailable(m)
        | AppError::UpstreamRefused(m)
        | AppError::Duplicate(m)
        | AppError::Validation(m)
        | AppError::UnauthorizedReason(m) => m.clone(),
        // A database failure's own text names the schema, so it is never reported: the generic
        // sentence this app answers the client with is the whole message (see `AppError::Database`).
        AppError::Database(_) => "Database error".to_string(),
        // A paused sign-in never reaches this bulk path (it is answered on the login route), but the
        // match must stay exhaustive: the refusal's own sentence is the throttle's, not a bare code.
        AppError::TooManyAttempts { retry_after_secs } => format!(
            "too many failed sign-in attempts — try again in {}s",
            retry_after_secs
        ),
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

/// POST /api/v1/admin/tenants/:id/suspension — shut a workspace, or open it again (2026-11-01,
/// cron `default`).
///
/// WHY THIS EXISTS: `tenants.is_active` was the only "close this workspace" lever in the schema and
/// NOTHING read it — not `auth::handlers::login`, not `auth::middleware` — so the flag was
/// decorative: it could be set and the customer kept working. The readers are now in place; this is
/// their writer, so the operator gets the middle option between "carry on" and "delete their data".
///
/// Suspending also ends every LIVE session of the workspace (`users.sessions_valid_from = NOW()`).
/// Without that, closing a door only binds the people who had not walked through yet — the session
/// already inside the product is exactly the one being shut out.
///
/// `guard_protected` applies, like every other operator write: a portfolio workspace or the
/// operator's own can never be suspended, so the console cannot lock the platform out of itself.
/// Re-opening restores access; nothing is deleted either way.
pub async fn set_tenant_suspension(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(tenant_id_str): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let tenant_id = Uuid::parse_str(&tenant_id_str)
        .map_err(|_| AppError::BadRequest("Invalid tenant ID".into()))?;
    // Required, not defaulted: an endpoint that guesses which way to flip a door is an endpoint
    // that flips it the wrong way on an empty body.
    let suspended = body
        .get("suspended")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| AppError::BadRequest("suspended (true|false) required".into()))?;

    guard_protected(&state, tenant_id, claims.aid).await?;

    let updated = sqlx::query(
        "UPDATE tenants SET is_active = $1, updated_at = NOW() WHERE id = $2 RETURNING id",
    )
    .bind(!suspended)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    if updated.is_none() {
        return Err(AppError::NotFound("Tenant not found".into()));
    }

    if suspended {
        // Best-effort by design: the workspace is already closed by the UPDATE above. A failed
        // revocation is logged loudly rather than turning the close into an error the operator has
        // to puzzle over — the flag is the record, and re-running the call retries it.
        if let Err(e) =
            sqlx::query("UPDATE users SET sessions_valid_from = NOW() WHERE tenant_id = $1")
                .bind(tenant_id)
                .execute(&state.pool)
                .await
        {
            tracing::error!(tenant_id = %tenant_id, error = %e, "suspended workspace but could not revoke its sessions");
        }
        tracing::warn!(
            tenant_id = %tenant_id,
            operator = %claims.sub,
            "workspace SUSPENDED by an operator"
        );
    } else {
        tracing::info!(
            tenant_id = %tenant_id,
            operator = %claims.sub,
            "workspace re-opened by an operator"
        );
    }

    Ok(Json(json!({
        "tenant_id": tenant_id_str,
        "suspended": suspended,
        "is_active": !suspended
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

/// `GET /api/v1/admin/stats` — the OPERATOR's platform-wide figures (cron 2026-10-20).
///
/// WHY THIS EXISTS. The operator console's landing screen read `/api/v1/dashboard/stats`, whose
/// every count is scoped to `claims.aid` — the OPERATOR'S OWN workspace — and carried the caption
/// "Counts are this workspace's own." Measured live on this app: a platform admin signing in to the
/// admin console saw Response Rules / Templates / Contacts / Missed Calls all at 0 while the fleet
/// held real workspaces, and the one number that makes money — how many customers are waiting for
/// a plan — appeared nowhere on the landing screen. An operator could file a plan request from a
/// customer and never be told it existed without opening the Tickets screen by hand.
///
/// The route sits under `/api/v1/admin/`, so `auth::middleware` has already refused a tenant token
/// before this handler runs; it reads only aggregate counts, never another workspace's rows.
///
/// HARNESS TRAFFIC IS NOT A CUSTOMER (measured live 2026-10-21, same class as FunnelSwift d75f0b4).
/// `admin_handler::list_all_tenants` below already hides a harness-tagged workspace
/// (`tenants.probe_harness`, stamped at signup by `crate::probe_harness::for_signup`, migration
/// 000038). These figures did not, so the landing screen said 7 workspaces while the Tenants screen
/// underneath listed 6, and the operator had no way to tell which number was true. `workspaces` and
/// `users` now use the SAME predicate as the list: tagged rows excluded unless `?include_probe=1`,
/// a pre-column NULL row is a customer, `^CNRY` canaries stay visible. The call counters need no
/// change — a probe workspace produces no calls, and hiding its rows there would misstate volume.
///
/// AND NEITHER IS A WORKSPACE NOBODY CAN SIGN INTO (measured live 2026-11-01). `list_all_tenants`
/// now also holds back a workspace with zero users (nobody can sign in, so it cannot be a
/// customer), so counting from a second spelling of the rules would re-create the very divergence
/// the paragraph above was written to end. Measured live 2026-11-01: this function answered
/// `workspaces: 6` while the list under it served 3 — the two nightly canaries (hidden by owner
/// ADDRESS, a Rust predicate with no SQL twin) and one zero-user leftover. The figures are
/// therefore counted from `is_hidden_workspace`, the SAME predicate the list filters with.
///
/// `users` counts the people in the VISIBLE workspaces only, for the same reason: the canaries each
/// carry a user row, and a landing screen that counts a probe workspace's user while refusing to
/// count the workspace is the same divergence one column over.
/// One row of the plan-value read used by the revenue card: (tenant, slug, name,
/// monthly price, yearly price, plain price, billing term). A named alias so the tuple type does
/// not trip clippy's type-complexity lint.
type PlanValueRow = (
    Uuid,
    String,
    String,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    String,
);

pub async fn platform_stats(
    State(state): State<AppState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, AppError> {
    let include_probe = params
        .get("include_probe")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let workspace_rows: Vec<(Uuid, String, Option<String>, i64)> = sqlx::query_as(
        "SELECT t.id, COALESCE(t.probe_harness, '') AS probe_harness, \
                (SELECT u.email FROM users u WHERE u.tenant_id = t.id \
                 ORDER BY CASE WHEN u.role IN ('account_owner','company_admin','admin','super_admin','agency_admin') \
                               THEN 0 ELSE 1 END, \
                          u.created_at ASC LIMIT 1) AS owner_email, \
                (SELECT COUNT(*) FROM users u WHERE u.tenant_id = t.id)::bigint AS user_count \
         FROM tenants t",
    )
    .fetch_all(&state.pool)
    .await?;
    let visible: Vec<&(Uuid, String, Option<String>, i64)> = workspace_rows
        .iter()
        .filter(|(_, tag, owner, users)| {
            include_probe || !is_hidden_workspace(tag, owner.as_deref(), *users)
        })
        .collect();
    let workspaces = visible.len() as i64;
    let users_visible: i64 = visible.iter().map(|(_, _, _, n)| *n).sum();
    let visible_ids: std::collections::HashSet<Uuid> =
        visible.iter().map(|(id, _, _, _)| *id).collect();

    // ── RECURRING REVENUE (cron 2026-11-09) ──────────────────────────────────────────────────
    // The dashboard counted workspaces and calls but could not say what the install EARNS. Two
    // kinds of workspace hold a paid-priced tier without paying for it: the operator's own and the
    // portfolio workspaces (the fleet gives them the top plan), and any hand-granted plan. They are
    // NOT hidden — they are reported beside the paid figure, because a revenue card that quietly
    // folded them in would overstate the business, and one that dropped them silently would hide
    // who is on the house. "Paying" is `billing_cycle IN ('monthly','yearly')`: the signup door
    // writes 'free' and a hand switch writes 'manual', so only a real billing term counts. The
    // tenant set is the SAME visible set the counters above use, so this card can never disagree
    // with the Tenants screen. Value is LIST PRICE, monthly-equivalent (a yearly plan counts as its
    // annual price over twelve months) — labelled as list price in the console, because no money
    // has been collected until a provider is connected.
    let plan_rows: Vec<PlanValueRow> = sqlx::query_as(
            "SELECT tp.tenant_id, p.slug, p.name,                     p.price_monthly::float8, p.price_yearly::float8, p.price::float8,                     COALESCE(tp.billing_cycle, '')                FROM tenant_plans tp JOIN plans p ON p.id = tp.plan_id               WHERE tp.is_active",
        )
        .fetch_all(&state.pool)
        .await?;
    let mut plan_value_monthly = 0.0f64;
    let mut paying_workspaces = 0i64;
    let mut house_workspaces = 0i64;
    let mut mix: Vec<(String, String, i64, i64, f64)> = Vec::new();
    for (tenant_id, slug, name, pm, py, price, cycle) in plan_rows {
        if !visible_ids.contains(&tenant_id) {
            continue;
        }
        let monthly = if cycle.eq_ignore_ascii_case("yearly") {
            py.or(pm).or(price).unwrap_or(0.0) / 12.0
        } else {
            pm.or(price).unwrap_or(0.0)
        };
        let paying = monthly > 0.0
            && (cycle.eq_ignore_ascii_case("monthly") || cycle.eq_ignore_ascii_case("yearly"));
        if paying {
            paying_workspaces += 1;
            plan_value_monthly += monthly;
        } else if monthly > 0.0 {
            house_workspaces += 1;
        }
        match mix.iter_mut().find(|m| m.0 == slug) {
            Some(m) => {
                m.2 += 1;
                if paying {
                    m.3 += 1;
                    m.4 += monthly;
                }
            }
            None => mix.push((
                slug,
                name,
                1,
                if paying { 1 } else { 0 },
                if paying { monthly } else { 0.0 },
            )),
        }
    }
    let round2 = |v: f64| (v * 100.0).round() / 100.0;
    let plan_mix_json: Vec<Value> = mix
        .iter()
        .map(|(slug, name, n, paying, monthly)| {
            serde_json::json!({
                "slug": slug,
                "name": name,
                "workspaces": n,
                "paying": paying,
                "monthly": round2(*monthly),
            })
        })
        .collect();

    let row: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT COUNT(*) FROM inbound_calls)::bigint, \
           (SELECT COUNT(*) FROM inbound_calls WHERE disposition = 'missed')::bigint, \
           (SELECT COUNT(*) FROM tickets WHERE status NOT IN ('resolved','closed'))::bigint, \
           (SELECT COUNT(*) FROM tickets WHERE source IN ('plan_request','billing') \
              AND status NOT IN ('resolved','closed'))::bigint",
    )
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(json!({
        "workspaces": workspaces,
        "users": users_visible,
        "calls_received": row.0,
        "missed_calls": row.1,
        "open_tickets": row.2,
        "plan_requests_waiting": row.3,
        // What the install EARNS, and who is on the house (cron 2026-11-09).
        "plan_value_monthly": round2(plan_value_monthly),
        "plan_value_yearly": round2(plan_value_monthly * 12.0),
        "paying_workspaces": paying_workspaces,
        "house_workspaces": house_workspaces,
        "plan_mix": plan_mix_json,
    })))
}

/// The ONE predicate that decides whether a workspace is fleet machinery rather than a customer.
/// `list_all_tenants` holds such rows back from its default answer and `platform_stats` counts
/// without them, so the landing screen can never disagree with the Tenants screen underneath it.
/// Three arms, each one measured: a harness tag the signup door stamped, an owner address on a
/// reserved / fleet-harness domain, and a workspace with NO user at all (nobody can sign in, so no
/// signup can be attributed to it). `^CNRY` canaries stay visible: they are the proof the nightly
/// sweep works.
fn is_hidden_workspace(probe_harness: &str, owner_email: Option<&str>, user_count: i64) -> bool {
    let tag = probe_harness.trim();
    let tagged = !tag.is_empty() && !tag.to_ascii_uppercase().starts_with("CNRY");
    let reserved = owner_email.map(owner_is_fleet_machinery).unwrap_or(false);
    tagged || reserved || user_count <= 0
}
