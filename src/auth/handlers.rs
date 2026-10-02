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

    let existing = sqlx::query_as::<_, TeamMember>("SELECT * FROM users WHERE lower(email) = $1")
        .bind(&email)
        .fetch_optional(&state.pool)
        .await?;

    if existing.is_some() {
        return Err(AppError::Conflict(
            "A user with this email already exists. Try signing in.".into(),
        ));
    }

    let account_id = uuid::Uuid::new_v4();
    let account_slug = req.account_name.to_lowercase().replace(' ', "_");

    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(account_id)
        .bind(&req.account_name)
        .bind(&account_slug)
        .execute(&state.pool)
        .await?;

    let user_id = uuid::Uuid::new_v4();
    let password_hash =
        hash_password(&req.password).map_err(|e| AppError::Internal(e.to_string()))?;
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO users (id, email, password_hash, name, tenant_id, role, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(user_id)
    .bind(&email)
    .bind(&password_hash)
    .bind(&req.name)
    .bind(account_id)
    .bind("account_owner")
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    // Auto-assign Free plan with 50 starter credits
    let free_plan = sqlx::query_as::<_, (uuid::Uuid,)>(
        "SELECT id FROM plans WHERE slug = 'free' AND is_active = true LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await?;

    if let Some((plan_id,)) = free_plan {
        let tp_id = uuid::Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO tenant_plans (id, tenant_id, plan_id, credit_balance, lifetime_credits, status, billing_cycle)
               VALUES ($1, $2, $3, 50, 50, 'active', 'free')"#
        )
        .bind(tp_id)
        .bind(account_id)
        .bind(plan_id)
        .execute(&state.pool)
        .await?;
    }

    let claims = Claims {
        sub: user_id,
        email: email.clone(),
        aid: account_id,
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

    // Send welcome email
    let wl_pool = state.pool.clone();
    let wl_email = email.clone();
    let wl_name = req.name.clone();
    tokio::spawn(async move {
        let vars = serde_json::json!({
            "name": wl_name,
            "email": wl_email,
            "app_name": "MissedCall Respondr",
            "login_url": "https://app.missedcallrespondr.com"
        });
        let nil = uuid::Uuid::nil();
        if let Err(e) =
            crate::email::send_template_email(&wl_pool, nil, &wl_email, "welcome", &vars).await
        {
            // `error!`, not `warn!`: the account exists either way, so the ONLY signal that the
            // customer got no mail is this line. Before kanban t_6d575da6 the transport itself was
            // broken (JSON to a form API) and this line was a `warn!` nobody read, which is how the
            // defect stayed invisible until a registration probe found it.
            tracing::error!(
                "account created but the WELCOME EMAIL FAILED for {} — the customer has no welcome/credentials mail: {}",
                wl_email,
                e
            );
        }
    });

    Ok(Json(AuthResponse {
        token,
        team_member: TeamMemberResponse {
            id: user_id,
            email,
            name: req.name,
            tenant_id: account_id,
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
