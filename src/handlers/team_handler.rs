//! The customer's own "Your team" door — list, invite, remove (2026-10-31).
//!
//! Until this module a MissedCall workspace had exactly ONE login: the `account_owner` row signup
//! minted. A second person (a receptionist, a partner, an office manager) could not be given access
//! at all — every colleague shared one password, and the only other way to create a `users` row was
//! the cross-app `portfolio_sync` door, which also mints a TENANT. The tenant console has counted
//! `users` against the plan's `max_users` since the usage payload was fixed, so the allowance was
//! readable and unspendable.
//!
//! Three rules sit in front of the row, each before anything is written:
//!   * AUTHORITY — the caller must administer THIS workspace: its `account_owner` (the row signup
//!     wrote), an appointed `company_admin`, or a platform admin. The tenant comes from the SESSION
//!     (`claims.aid`), never from the body, so a session cannot mint into another workspace.
//!   * ALLOWANCE — `features::enforce_feature_limit("max_users")` runs BEFORE the insert, so the
//!     plan's seat cap refuses instead of leaving a login the plan does not allow.
//!   * VOCABULARY — the invited row is `business_user`, the role this app already uses for a
//!     workspace member who is not the owner. `admin`/`super_admin` are the PLATFORM operator roles
//!     (`middleware::is_platform_admin`) and are never handed out here.
//!
//! The mint itself is not a new writer of the account shape: the row is the same `users` INSERT the
//! signup writes (same columns, same argon2 hash, same `welcome_credentials` mail carrying the
//! generated password), with an existing tenant instead of a new one.

use axum::{
    extract::{Extension, Path, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{auth::models::hash_password, config::Claims, error::AppError, state::AppState};

/// Workspace roles that may add or remove a teammate.
const WORKSPACE_ADMIN_ROLES: [&str; 2] = ["account_owner", "company_admin"];
/// The role a teammate is minted with: a member of the workspace, never a platform operator.
const TEAMMATE_ROLE: &str = "business_user";

fn may_administer(role: &str) -> bool {
    WORKSPACE_ADMIN_ROLES.contains(&role) || crate::auth::middleware::is_platform_admin(role)
}

fn refuse_non_admin(role: &str) -> AppError {
    // 403, not 401: the credential is valid, it just does not administer this workspace.
    AppError::Forbidden(format!(
        "Only the workspace owner or a company admin can change who has access (your role: {role})."
    ))
}

/// The workspace's seats: how many people there are, and what the plan allows when it declares a
/// number. `limit: null` is the fleet's absence rule ("the plan declares no allowance"), which the
/// console renders as no cap rather than as "0 allowed".
async fn seats(state: &AppState, tenant_id: Uuid) -> (i64, Option<i64>) {
    let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);
    let limit = match crate::features::active_plan_slug(&state.pool, tenant_id).await {
        Some(slug) => crate::features::resolve_limit(&state.pool, &slug, "max_users")
            .await
            .ok()
            .and_then(|(v, _)| v),
        None => None,
    };
    (used, limit)
}

/// GET /api/v1/team/members — the people in the caller's own workspace, oldest first (the first row
/// is the account owner signup wrote).
pub async fn list_members(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let rows = sqlx::query_as::<_, (Uuid, String, String, String, chrono::NaiveDateTime)>(
        "SELECT id, name, email, role, created_at FROM users \
         WHERE tenant_id = $1 ORDER BY created_at ASC, id ASC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;

    let owner_id = rows.first().map(|r| r.0);
    let members: Vec<Value> = rows
        .iter()
        .map(|(id, name, email, role, created_at)| {
            json!({
                "id": id,
                "name": name,
                "email": email,
                "role": role,
                "is_owner": Some(*id) == owner_id,
                "is_you": *id == claims.sub,
                "created_at": created_at,
            })
        })
        .collect();
    let (used, limit) = seats(&state, claims.aid).await;
    Ok(Json(json!({
        "members": members,
        "seats": { "used": used, "limit": limit },
        "can_manage": may_administer(&claims.role),
    })))
}

#[derive(Deserialize)]
pub struct InviteRequest {
    pub name: Option<String>,
    pub email: String,
}

/// POST /api/v1/team/invite — mint a login for one more person in the caller's workspace and mail
/// them their first password.
pub async fn invite_teammate(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<InviteRequest>,
) -> Result<Json<Value>, AppError> {
    if !may_administer(&claims.role) {
        return Err(refuse_non_admin(&claims.role));
    }
    // The fleet's ONE address rule, before the first write and before the seat gate: the normalised
    // value is what gets stored and what the mail is sent to.
    let email =
        crate::security::email_addr::normalize(&req.email).map_err(AppError::Unprocessable)?;
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| email.split('@').next().unwrap_or("Teammate").to_string());
    crate::validation::check_len("name", &name, 255)?;

    // Duplicate check BEFORE the seat gate and before the argon2 work: an address that is already a
    // login is a 409 whether or not a seat happens to be free.
    if crate::auth::signup::email_taken(&state.pool, &email).await? {
        return Err(AppError::Conflict(
            "That address already has a login in this app. Ask them to sign in, or use a different address."
                .into(),
        ));
    }

    // The plan's seat allowance, enforced before the row (a user row plus a credentials mail for a
    // login the plan does not allow is worse than a refusal).
    crate::features::enforce_feature_limit(&state.pool, claims.aid, "max_users", "Team members")
        .await?;

    let password = crate::auth::signup::generate_temp_password();
    let password_hash = hash_password(&password)
        .map_err(|e| AppError::Internal(format!("could not hash the invited password: {e}")))?;
    let user_id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO users (id, email, password_hash, name, tenant_id, role, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(user_id)
    .bind(&email)
    .bind(&password_hash)
    .bind(&name)
    .bind(claims.aid)
    .bind(TEAMMATE_ROLE)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e {
            if db_err.constraint() == Some("users_email_key") {
                // Lost the race with another invite of the same address: still a duplicate, never a 500.
                return AppError::Conflict(
                    "That address already has a login in this app. Ask them to sign in, or use a different address."
                        .into(),
                );
            }
        }
        AppError::from(e)
    })?;

    // The credentials mail — awaited, like the tag door: it is the ONLY delivery of this password, so
    // its failure has to be reported to the inviter instead of looking like a successful invite.
    let vars = json!({ "name": name, "email": email, "password": password });
    let credentials_emailed = match crate::email::send_template_email(
        &state.pool,
        claims.aid,
        &email,
        "welcome_credentials",
        &vars,
    )
    .await
    {
        Ok(()) => true,
        Err(e) => {
            tracing::error!(
                "teammate {} ({}) was created in tenant {} but the CREDENTIALS EMAIL FAILED: {}",
                user_id,
                email,
                claims.aid,
                e
            );
            false
        }
    };

    let (used, limit) = seats(&state, claims.aid).await;
    Ok(Json(json!({
        "member": {
            "id": user_id,
            "name": name,
            "email": email,
            "role": TEAMMATE_ROLE,
            "is_owner": false,
            "is_you": false,
            "created_at": now,
        },
        "credentials_emailed": credentials_emailed,
        "seats": { "used": used, "limit": limit },
        "detail": if credentials_emailed {
            "Their login has been created and their password is on its way to that address.".to_string()
        } else {
            "Their login was created, but the email carrying the password did not leave. Use Send reset on the operator console, or ask them to use Forgot password once mail delivery is restored.".to_string()
        },
    })))
}

/// DELETE /api/v1/team/members/:id — revoke one person's access.
///
/// Deletion IS the revocation in this app (`sessions_valid_from` is not consulted on delete), so the
/// row goes: the token they hold stops resolving a user on its next request. The account owner (the
/// oldest row, the one signup wrote) can never be removed here — a workspace with no owner has no
/// door to add one back — and nobody can remove themselves with this control.
pub async fn remove_member(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    if !may_administer(&claims.role) {
        return Err(refuse_non_admin(&claims.role));
    }
    if id == claims.sub {
        return Err(AppError::BadRequest(
            "That is your own login. Sign out instead of removing yourself from the workspace."
                .into(),
        ));
    }
    let target: Option<(String, String)> =
        sqlx::query_as("SELECT email, role FROM users WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?;
    let (target_email, _target_role) = target.ok_or_else(|| {
        // 404, not 403: a row in another workspace must not be confirmed to exist.
        AppError::NotFound("No such teammate in this workspace.".into())
    })?;
    let owner_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users WHERE tenant_id = $1 ORDER BY created_at ASC, id ASC LIMIT 1",
    )
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?;
    if owner_id == Some(id) {
        return Err(AppError::BadRequest(
            "The workspace owner's login cannot be removed. Transfer ownership first.".into(),
        ));
    }
    sqlx::query("DELETE FROM users WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;
    let (used, limit) = seats(&state, claims.aid).await;
    Ok(Json(json!({
        "removed": true,
        "email": target_email,
        "seats": { "used": used, "limit": limit },
    })))
}
