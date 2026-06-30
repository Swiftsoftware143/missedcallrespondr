use axum::{
    extract::{Extension, State},
    Json,
};
use serde_json::{json, Value};
use uuid::Uuid;
use argon2::{Argon2, PasswordHasher};
use argon2::password_hash::SaltString;
use rand::rngs::OsRng;

use crate::config::Claims;
use crate::error::AppError;
use crate::state::AppState;
use crate::auth::models::{create_token, validate_token};

/// Admin sync endpoint called by CoreSwift
pub async fn portfolio_sync(
    State(state): State<AppState>,
    Json(req): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let sync_id = req.get("id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok()).unwrap_or_else(Uuid::new_v4);
    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("Company").to_string();
    let email = req.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let description = req.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();

    if email.is_empty() {
        return Err(AppError::BadRequest("email is required".into()));
    }

    // Check email uniqueness
    let existing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE email = $1")
        .bind(&email)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(0);

    if existing > 0 {
        return Err(AppError::Conflict(format!("A user with email {} already exists", email)));
    }

    // Create tenant
    let tenant_id = uuid::Uuid::new_v4();
    let tenant_slug = name.to_lowercase().replace(' ', "_");

    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(&name)
        .bind(&tenant_slug)
        .execute(&state.pool)
        .await?;

    // Create user
    let user_id = uuid::Uuid::new_v4();
    let generated_password = Uuid::new_v4().to_string().replace("-", "").chars().take(12).collect::<String>();
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(generated_password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(e.to_string()))?
        .to_string();

    // Check for duplicate email
    let email_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE email = $1)",
    )
    .bind(&email)
    .fetch_one(&state.pool)
    .await
    .unwrap_or(false);

    if email_exists {
        return Err(AppError::BadRequest(format!(
            "User with email '{}' already exists", email
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
    .bind(now.clone())
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
        "tenant_id": tenant_id.to_string(),
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
    if claims.role != "agency_admin" {
        return Err(AppError::Unauthorized("Only agency admins can impersonate".into()));
    }

    let target_tenant_id = req.get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("valid tenant_id is required".into()))?;

    let now = chrono::Utc::now().timestamp() as usize;
    let imp_claims = Claims {
        sub: claims.sub,
        email: format!("impersonated@{}", target_tenant_id),
        tenant_id: target_tenant_id,
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
        "message": "Full tenant switch. Admin panel disappears."
    })))
}

/// Stop impersonation
pub async fn stop_impersonation() -> Result<Json<Value>, AppError> {
    Ok(Json(json!({
        "status": "impersonation_stopped",
        "note": "Drop impersonation token. Restore admin token."
    })))
}
