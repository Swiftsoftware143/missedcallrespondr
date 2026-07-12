use axum::{
    extract::{Extension, Path, State},
    Json,
};
use serde_json::{json, Value};
use uuid::Uuid;
use argon2::{Argon2, PasswordHasher};
use argon2::password_hash::SaltString;
use rand::rngs::OsRng;

use chrono::{DateTime, Utc};
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

/// List all tenants (for super admin)
pub async fn list_all_tenants(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, AppError> {
    // Simple auth check — verify token has admin access
    let auth_header = headers.get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| AppError::Unauthorized("Missing auth token".into()))?;
    
    // Validate token
    let claims = crate::auth::models::validate_token(auth_header, &state.config.jwt_secret)
        .map_err(|_| AppError::Unauthorized("Invalid token".into()))?;
    
    if claims.role != "agency_admin" && claims.role != "admin" && claims.role != "super_admin" {
        return Err(AppError::Unauthorized("Not authorized to list tenants".into()));
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
        ORDER BY t.created_at DESC"#
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
    let amount = body.get("amount")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| AppError::BadRequest("amount (integer) required".into()))? as i32;
    
    if amount <= 0 {
        return Err(AppError::BadRequest("Amount must be positive".into()));
    }
    
    // Upsert tenant_plan with credit increase
    let existing = sqlx::query(
        "SELECT credit_balance FROM tenant_plans WHERE tenant_id = $1"
    )
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
        },
        None => {
            // Try to find any plan to associate, or use a dummy fallback
            let plan = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM plans ORDER BY sort_order ASC LIMIT 1"
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
                return Err(AppError::Internal("No plans exist. Create a plan first.".into()));
            }
        }
    }
    
    Ok(Json(json!({
        "message": format!("Added {} credits", amount),
        "tenant_id": tenant_id_str
    })))
}

/// Admin: delete a tenant and all associated data
pub async fn delete_tenant(
    State(state): State<AppState>,
    Path(tenant_id_str): Path<String>,
) -> Result<Json<Value>, AppError> {
    let tenant_id = Uuid::parse_str(&tenant_id_str)
        .map_err(|_| AppError::BadRequest("Invalid tenant ID".into()))?;

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
