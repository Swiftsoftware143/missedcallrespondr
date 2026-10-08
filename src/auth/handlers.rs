use axum::{
    extract::{Extension, State},
    Json,
};

use serde_json::Value;

use super::models::{create_token, hash_password, verify_password};
use crate::{
    config::{
        AuthResponse, ChangePasswordRequest, Claims, ForgotPasswordRequest, LoginRequest,
        RegisterRequest, ResetPasswordRequest, TeamMember, TeamMemberResponse,
    },
    error::AppError,
    security::email_addr,
    state::AppState,
};

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    // ── Address boundary (kanban t_54b1ffab) ────────────────────────────────────────────────
    // FIRST, before any SELECT and long before any INSERT. `users.email` is both the login identity
    // and the only address the welcome/credentials mail can ever reach; the handler used to bind
    // `req.email` verbatim, so the literal string `bad` became a real account that no mail could
    // ever be delivered to. Normalises (trim + lowercase) as well as validates, and the normalised
    // value is what is checked, stored, put in the token and mailed.
    let email = email_addr::normalize(&req.email).map_err(AppError::Unprocessable)?;

    // The duplicate check runs FIRST, before the Argon2 work: `register` is reachable without a
    // credential, and a hash nobody will store must not be computed for a duplicate (the shared
    // core re-checks before its first write, so this is the cheap first gate, not the only one).
    if super::signup::email_taken(&state.pool, &email).await? {
        return Err(AppError::Conflict(
            "A user with this email already exists. Try signing in.".into(),
        ));
    }

    // David's signup model (as IncentiveSwift/FunnelSwift ship): the page collects NAME + EMAIL
    // only, so `password` may arrive empty. The server then mints one and mails it; the user
    // confirms their address by signing in with it. A caller that still supplies one is honoured.
    let password = if req.password.is_empty() {
        super::signup::generate_temp_password()
    } else {
        req.password.clone()
    };

    let password_hash = hash_password(&password).map_err(|e| AppError::Internal(e.to_string()))?;

    // ── The mint, through the ONE shared writer (kanban t_1d08bd9a, design §3.1 rule 4) ─────────
    // `auth::signup::create_account` is the same function the fleet-internal tag door
    // (`POST /api/v1/internal/provision-free-account`) calls, so the account this public signup
    // mints and the account a FunnelSwift tag mints cannot drift: one `tenants` row, one
    // `account_owner` `users` row, one `tenant_plans` row on `free` with 50 starter credits.
    // `account_slug: None` makes `create_account` DERIVE a not-taken slug through
    // `signup::unique_account_slug` (kanban t_1a26f923 — the raw name derivation used to collide on
    // `tenants_slug_key` and 500 the second visitor of the same name); `password_plain` now
    // carries the server-minted first password so the `welcome_credentials` mail can deliver it.
    // No workspace-name field on the page: derive "<name>'s Workspace" when none was supplied.
    let account_name = {
        let a = req.account_name.trim();
        if a.is_empty() {
            format!("{}'s Workspace", req.name)
        } else {
            a.to_string()
        }
    };
    let ids = super::signup::create_account(
        &state,
        super::signup::NewAccount {
            email: &email,
            name: &req.name,
            password_hash: &password_hash,
            // The server-minted plaintext is the ONLY delivery of that password, so the
            // `welcome_credentials` template carries it (the tag door's existing posture).
            password_plain: Some(&password),
            account_name: &account_name,
            account_slug: None,
            plan_slug: "free",
            role: "account_owner",
        },
    )
    .await?;

    let claims = Claims {
        sub: ids.user_id,
        email: email.clone(),
        aid: ids.account_id,
        role: "account_owner".into(),
        exp: (chrono::Utc::now().timestamp() + 86400 * 7) as usize,
        iat: chrono::Utc::now().timestamp() as usize,
    };

    let token = create_token(&claims, &state.config.jwt_secret)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // NOTE: the legacy signup -> CoreSwift "cross-app/tag-sync" push was removed here.
    // It was provably dead: CoreSwift's handler requires `lead.id` (TagSyncLead.id is a
    // non-optional String) while this payload only ever sent name+email, so every signup
    // got HTTP 422 and no hub row was ever written. It also authenticated with the
    // fleet-wide INTERNAL_SYNC_KEY env secret, which the platform standard forbids.
    // The live inbound hub path is handlers::coreswift_external::push_lead_to_coreswift
    // (tenant BYOK csk_ key), fired on real captures - not on signup.

    Ok(Json(AuthResponse {
        token,
        team_member: TeamMemberResponse {
            id: ids.user_id,
            email,
            name: req.name,
            tenant_id: ids.account_id,
            role: "account_owner".into(),
        },
    }))
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    // The same normalisation `register` stores by, matched case-insensitively so an account stored
    // with capitals (or created before normalisation existed — live has `Zaarhub@gmail.com`) still
    // resolves when the customer retypes their address with different casing. A malformed value is
    // NOT refused here: login answers its own 401 for every wrong credential, and it must not become
    // an account-existence oracle. It simply matches nothing.
    let user = sqlx::query_as::<_, TeamMember>("SELECT * FROM users WHERE lower(email) = $1")
        .bind(email_addr::lookup_key(&req.email))
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::Unauthorized("Invalid email or password".into()))?;

    let valid = verify_password(&req.password, &user.password_hash)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if !valid {
        return Err(AppError::Unauthorized("Invalid email or password".into()));
    }

    let claims = Claims {
        sub: user.id,
        email: user.email.clone(),
        aid: user.tenant_id,
        role: user.role.clone(),
        exp: (chrono::Utc::now().timestamp() + 86400 * 7) as usize,
        iat: chrono::Utc::now().timestamp() as usize,
    };

    let token = create_token(&claims, &state.config.jwt_secret)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(AuthResponse {
        token,
        team_member: user.into(),
    }))
}

pub async fn me(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<TeamMemberResponse>, AppError> {
    let user = sqlx::query_as::<_, TeamMember>("SELECT * FROM users WHERE id = $1")
        .bind(claims.sub)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    Ok(Json(user.into()))
}

pub async fn change_password(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    if req.new_password.len() < 8 {
        return Err(AppError::BadRequest(
            "New password must be at least 8 characters".into(),
        ));
    }

    let user = sqlx::query_as::<_, TeamMember>("SELECT * FROM users WHERE id = $1")
        .bind(claims.sub)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::Unauthorized("User not found".into()))?;

    let valid = verify_password(&req.current_password, &user.password_hash)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !valid {
        return Err(AppError::Unauthorized(
            "Current password is incorrect".into(),
        ));
    }

    let new_hash =
        hash_password(&req.new_password).map_err(|e| AppError::Internal(e.to_string()))?;

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(&new_hash)
        .bind(user.id)
        .execute(&state.pool)
        .await?;

    Ok(Json(
        serde_json::json!({"message": "Password updated successfully"}),
    ))
}

pub async fn update_profile(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("name is required".into()))?;

    if name.trim().is_empty() {
        return Err(AppError::BadRequest("name cannot be empty".into()));
    }

    sqlx::query("UPDATE users SET name = $1, updated_at = NOW() WHERE id = $2")
        .bind(name)
        .bind(claims.sub)
        .execute(&state.pool)
        .await?;

    Ok(Json(
        serde_json::json!({"message": "Profile updated", "name": name}),
    ))
}

pub async fn forgot_password(
    State(state): State<AppState>,
    Json(req): Json<ForgotPasswordRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // The SAME boundary rule as `register`, from the same function: an address that could never
    // receive the reset mail is refused with the same 422/field shape instead of silently
    // reporting "if the email exists…" for an address that cannot exist as a mailbox. This reply
    // is unconditional for every well-formed address, so it still leaks nothing about accounts.
    let email = email_addr::normalize(&req.email).map_err(AppError::Unprocessable)?;

    if let Some(user) =
        sqlx::query_as::<_, TeamMember>("SELECT * FROM users WHERE lower(email) = $1")
            .bind(&email)
            .fetch_optional(&state.pool)
            .await?
    {
        let token = uuid::Uuid::new_v4().to_string();
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(24);

        sqlx::query("UPDATE password_resets SET used = true WHERE user_id = $1 AND used = false")
            .bind(user.id)
            .execute(&state.pool)
            .await
            .ok();

        sqlx::query("INSERT INTO password_resets (user_id, token, expires_at) VALUES ($1, $2, $3)")
            .bind(user.id)
            .bind(&token)
            .bind(expires_at)
            .execute(&state.pool)
            .await?;

        // Send password reset email via template system
        let vars = serde_json::json!({
            "name": user.name,
            "token": token,
            "app_url": "https://app.missedcallrespondr.com",
        });
        match crate::email::send_template_email(
            &state.pool,
            uuid::Uuid::nil(),
            &user.email,
            "password_reset",
            &vars,
        )
        .await
        {
            Ok(_) => tracing::info!("Password reset email sent to {}", user.email),
            Err(e) => tracing::error!(
                "Failed to send password reset email to {}: {}",
                user.email,
                e
            ),
        }
    }

    Ok(Json(
        serde_json::json!({"message": "If the email exists, a password reset link has been sent"}),
    ))
}

pub async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    if req.new_password.len() < 8 {
        return Err(AppError::BadRequest(
            "New password must be at least 8 characters".into(),
        ));
    }

    use sqlx::Row;
    let reset = sqlx::query(
        "SELECT id, user_id FROM password_resets WHERE token = $1 AND used = false AND expires_at > NOW()",
    )
    .bind(&req.token)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::BadRequest("Invalid or expired reset token".into()))?;

    let reset_id: uuid::Uuid = reset.get("id");
    let user_id: uuid::Uuid = reset.get("user_id");

    let new_hash =
        hash_password(&req.new_password).map_err(|e| AppError::Internal(e.to_string()))?;

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(&new_hash)
        .bind(user_id)
        .execute(&state.pool)
        .await?;

    sqlx::query("UPDATE password_resets SET used = true WHERE id = $1")
        .bind(reset_id)
        .execute(&state.pool)
        .await?;

    Ok(Json(
        serde_json::json!({"message": "Password has been reset successfully"}),
    ))
}

pub async fn get_usage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>, AppError> {
    let tid = claims.aid;
    let usage = crate::features::get_usage_json(&state.pool, tid).await;
    Ok(Json(usage))
}
