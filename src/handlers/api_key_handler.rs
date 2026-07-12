use argon2::{
    password_hash::{rand_core::OsRng, SaltString, PasswordHasher},
    Argon2,
};
use axum::{
    extract::{Path, State, Extension},
    http::StatusCode,
    Json,
};
use rand::Rng;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::features;
use crate::config::Claims;
use crate::error::AppError;
use crate::state::AppState;

fn generate_api_key() -> (String, String) {
    let prefix = "missedca_".to_string();
    let random_part: String = (0..16)
        .map(|_| format!("{:x}", rand::thread_rng().gen_range(0..16)))
        .collect();
    let raw_key = format!("missedcallrespondr_{}", random_part);
    (raw_key, prefix)
}

fn hash_api_key(raw_key: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(raw_key.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| AppError::Internal(format!("Hash error: {e}")))
}

pub async fn create_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<serde_json::Value>), AppError> {
    let tenant_id: Uuid = claims.aid;
    features::enforce_feature_limit(&state.pool, tenant_id, "max_api_keys", "Api Keys").await?;
    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("default");
    let target_url = req.get("target_url").and_then(|v| v.as_str()).unwrap_or("");

    let (raw_key, prefix) = generate_api_key();
    let key_hash = hash_api_key(&raw_key)?;
    let id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO api_keys (id, tenant_id, user_id, name, key_hash, prefix, permissions, target_url) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"
    )
    .bind(id)
    .bind(claims.aid)
    .bind(claims.sub)
    .bind(name)
    .bind(&key_hash)
    .bind(&prefix)
    .bind(serde_json::json!([]))
    .bind(target_url)
    .execute(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "key": raw_key,
            "prefix": prefix,
            "name": name,
            "message": "Save this key - it will not be shown again"
        })),
    ))
}

pub async fn list_api_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<serde_json::Value>, AppError> {
    let rows = sqlx::query(
        "SELECT id::text, name, prefix, target_url, is_active, last_used_at::text, created_at::text FROM api_keys WHERE tenant_id = $1 ORDER BY created_at DESC"
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;

    let keys: Vec<serde_json::Value> = rows.iter().map(|row| {
        json!({
            "id": row.try_get::<&str, _>("id").unwrap_or(""),
            "name": row.try_get::<&str, _>("name").unwrap_or(""),
            "prefix": row.try_get::<&str, _>("prefix").unwrap_or(""),
            "target_url": row.try_get::<Option<&str>, _>("target_url").unwrap_or(None),
            "is_active": row.try_get::<bool, _>("is_active").unwrap_or(false),
        })
    }).collect();

    Ok(Json(json!({"api_keys": keys})))
}

pub async fn update_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM api_keys WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_one(&state.pool)
    .await?;

    if existing == 0 {
        return Err(AppError::NotFound("API key not found".into()));
    }

    let mut set_parts: Vec<String> = Vec::new();
    if let Some(name) = req.get("name").and_then(|v| v.as_str()) {
        set_parts.push(format!("name = '{}'", name.replace('\'', "''")));
    }
    if let Some(url) = req.get("target_url").and_then(|v| v.as_str()) {
        set_parts.push(format!("target_url = '{}'", url.replace('\'', "''")));
    }
    if let Some(active) = req.get("is_active").and_then(|v| v.as_bool()) {
        set_parts.push(format!("is_active = {}", active));
    }
    set_parts.push("updated_at = NOW()".to_string());

    if !set_parts.is_empty() {
        let sql = format!(
            "UPDATE api_keys SET {} WHERE id = '{}'",
            set_parts.join(", "),
            id
        );
        sqlx::query(&sql).execute(&state.pool).await?;
    }

    Ok(Json(json!({ "message": "API key updated", "id": id })))
}

pub async fn delete_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM api_keys WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("API key not found".into()));
    }

    Ok(Json(json!({ "message": "API key deleted", "id": id })))
}
