use crate::email::send_reset_email;
use axum::{
    extract::{Extension, State},
    Json,
};

use crate::{
    config::{AuthResponse, Claims, LoginRequest, RegisterRequest, TeamMember, TeamMemberResponse, ChangePasswordRequest, ForgotPasswordRequest, ResetPasswordRequest},
    error::AppError,
    state::AppState,
};
use super::models::{create_token, hash_password, verify_password};

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let existing = sqlx::query_as::<_, TeamMember>(
        "SELECT * FROM users WHERE email = $1",
    )
    .bind(&req.email)
    .fetch_optional(&state.pool)
    .await?;

    if existing.is_some() {
        return Err(AppError::Conflict("A user with this email already exists. Try signing in.".into()));
    }

    let account_id = uuid::Uuid::new_v4();
    let account_slug = req.account_name.to_lowercase().replace(' ', "_");

    sqlx::query(
        "INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)",
    )
    .bind(account_id)
    .bind(&req.account_name)
    .bind(&account_slug)
    .execute(&state.pool)
    .await?;

    let user_id = uuid::Uuid::new_v4();
    let password_hash = hash_password(&req.password).map_err(|e| AppError::Internal(e.to_string()))?;
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO users (id, email, password_hash, name, tenant_id, role, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(user_id)
    .bind(&req.email)
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
        "SELECT id FROM plans WHERE slug = 'free' AND is_active = true LIMIT 1"
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
        email: req.email.clone(),
        aid: account_id,
        role: "account_owner".into(),
        exp: (chrono::Utc::now().timestamp() + 86400 * 7) as usize,
        iat: chrono::Utc::now().timestamp() as usize,
    };

    let token = create_token(&claims, &state.config.jwt_secret)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(AuthResponse {
        token,
        team_member: TeamMemberResponse {
            id: user_id,
            email: req.email,
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
    let user = sqlx::query_as::<_, TeamMember>(
        "SELECT * FROM users WHERE email = $1",
    )
    .bind(&req.email)
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
    let user = sqlx::query_as::<_, TeamMember>(
        "SELECT * FROM users WHERE id = $1",
    )
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
        return Err(AppError::BadRequest("New password must be at least 8 characters".into()));
    }

    let user = sqlx::query_as::<_, TeamMember>(
        "SELECT * FROM users WHERE id = $1",
    )
    .bind(claims.sub)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::Unauthorized("User not found".into()))?;

    let valid = verify_password(&req.current_password, &user.password_hash)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !valid {
        return Err(AppError::Unauthorized("Current password is incorrect".into()));
    }

    let new_hash = hash_password(&req.new_password)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(&new_hash)
        .bind(user.id)
        .execute(&state.pool)
        .await?;

    Ok(Json(serde_json::json!({"message": "Password updated successfully"})))
}

pub async fn update_profile(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let name = req.get("name")
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
    
    Ok(Json(serde_json::json!({"message": "Profile updated", "name": name})))
}

pub async fn forgot_password(
    State(state): State<AppState>,
    Json(req): Json<ForgotPasswordRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    if let Some(user) = sqlx::query_as::<_, TeamMember>(
        "SELECT * FROM users WHERE email = $1",
    )
    .bind(&req.email)
    .fetch_optional(&state.pool)
    .await?
    {
        let token = uuid::Uuid::new_v4().to_string();
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(24);

        sqlx::query("UPDATE password_resets SET used = true WHERE user_id = $1 AND used = false")
            .bind(user.id)
            .execute(&state.pool)
            .await.ok();

        sqlx::query(
            "INSERT INTO password_resets (user_id, token, expires_at) VALUES ($1, $2, $3)",
        )
        .bind(user.id)
        .bind(&token)
        .bind(expires_at)
        .execute(&state.pool)
        .await?;

        // Send password reset email via SMTP
        match send_reset_email(&user.email, &token).await {
            Ok(_) => tracing::info!("Password reset email sent to {}", user.email),
            Err(e) => tracing::error!("Failed to send password reset email to {}: {}", user.email, e),
        }
    }

    Ok(Json(serde_json::json!({"message": "If the email exists, a password reset link has been sent"})))
}

pub async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    if req.new_password.len() < 8 {
        return Err(AppError::BadRequest("New password must be at least 8 characters".into()));
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

    let new_hash = hash_password(&req.new_password)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(&new_hash)
        .bind(user_id)
        .execute(&state.pool)
        .await?;

    sqlx::query("UPDATE password_resets SET used = true WHERE id = $1")
        .bind(reset_id)
        .execute(&state.pool)
        .await?;

    Ok(Json(serde_json::json!({"message": "Password has been reset successfully"})))
}
