//! API keys — the durable bearer credential MissedCall Respondr issues to every account
//! (ADASwift shape, kanban t_b6d650d6; re-introduced here by migration 000055 after the
//! 000027 retirement).
//!
//! David's standing requirement: EVERY account auto-mints an API key at creation
//! (`auth::signup::create_account` calls [`mint_on`] on the account's own transaction), the key
//! stays RETRIEVABLE (key_hash for auth + key_sealed for display, never hash-only), and the
//! settings screen offers View / Copy / Regenerate with an audit row per reveal.
//!
//! The key is stored twice:
//!   * `key_hash`   — hex SHA-256, the ONLY thing the auth middleware and the fleet MCP read;
//!   * `key_sealed` — the same key sealed with this app's provider-key envelope (`enc:v1:…`),
//!     pgp_sym_encrypt under PROVIDER_KEY_ENC_SECRET), used ONLY to display it back to its owner.
//!     Authentication never decrypts; reveal never authenticates.
//!
//! No `role` column: the key's authority is DERIVED from its owning user's live `users.role` at
//! resolve time ([`resolve_api_key`]), so a demoted owner's key loses access immediately and a
//! deleted owner's key dies with them — the same boundary the JWT path already enforces in
//! `auth::middleware`.
//!
//! Routes (tenant-scoped — every signed-in user manages THEIR workspace's keys, no admin gate):
//!   GET    /api/v1/api-keys             list this tenant's keys (never the plaintext)
//!   POST   /api/v1/api-keys             mint one — returns the plaintext (still viewable later);
//!                                       409 when a live key already exists (one per workspace)
//!   POST   /api/v1/api-keys/:id/reveal  show a key again — requires the caller's CURRENT password
//!   DELETE /api/v1/api-keys/:id         revoke one (soft: sets `revoked_at`)
//!   POST   /api/v1/api-keys/:id/regenerate  one atomic retire-then-mint; the old key 401s at once

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::config::Claims;
use crate::error::{ApiResult, AppError};
use crate::state::AppState;

/// Longest-lived credential this app issues: 100 years, so "durable" is literal and revocation is
/// the `revoked_at` column, never the clock.
const API_KEY_TTL_SECS: usize = 100 * 365 * 24 * 3600;

/// hex SHA-256 of the full plaintext token (64 chars). The ONLY form ever persisted for auth.
fn hash_key(token: &str) -> String {
    hex::encode(ring::digest::digest(
        &ring::digest::SHA256,
        token.as_bytes(),
    ))
}

/// Does a presented bearer look like one of this app's keys? The `mcr_` prefix announces the key
/// SHAPE before any database read; the verification itself stays in [`resolve_api_key`].
pub fn is_api_key(token: &str) -> bool {
    token
        .strip_prefix("mcr_")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A key that was just minted. `token` is the plaintext: sealed before the row is written, handed
/// to the caller's UI, never logged.
pub struct MintedKey {
    pub id: Uuid,
    pub name: String,
    pub token: String,
    pub prefix: String,
}

/// Mint one key and INSERT it on the caller's connection. The ONE writer for every mint in this
/// app: the signup transaction, `create_api_key` and `regenerate_api_key` all funnel through here.
///
/// The audit row (`api_key_created`) is written HERE, on the same connection as the insert, so a
/// mint and its audit row commit or roll back together (the invariant that survived verification
/// in ADASwift, kanban t_fb9dc70b).
pub async fn mint_on(
    conn: &mut sqlx::PgConnection,
    tenant_id: Uuid,
    created_by: Option<Uuid>,
    name: &str,
) -> Result<MintedKey, AppError> {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    let token = format!("mcr_{}", hex::encode(buf));
    let key_prefix: String = token.chars().take(12).collect();
    let key_hash = hash_key(&token);
    // Fail closed: without PROVIDER_KEY_ENC_SECRET this errors rather than storing a key nobody
    // can ever read back.
    let sealed = crate::security::provider_key_crypto::seal_in(&mut *conn, &token)
        .await
        .map_err(|e| AppError::Internal(format!("could not seal the API key: {e}")))?;
    let id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO api_keys (id, tenant_id, name, key_prefix, key_hash, key_sealed, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(name)
    .bind(&key_prefix)
    .bind(&key_hash)
    .bind(&sealed)
    .bind(created_by)
    .execute(&mut *conn)
    .await?;

    sqlx::query(
        "INSERT INTO activity_log (id, tenant_id, user_id, action, entity_type, entity_id, metadata) \
         VALUES ($1, $2, $3, 'api_key_created', 'api_key', $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(created_by)
    .bind(id)
    .bind(json!({ "key_prefix": key_prefix.clone(), "name": name, "source": "mint_on" }))
    .execute(&mut *conn)
    .await?;

    Ok(MintedKey {
        id,
        name: name.to_string(),
        token,
        prefix: key_prefix,
    })
}

/// Resolve a bearer token that is NOT a valid JWT against `api_keys`.
///
/// Returns synthesized [`Claims`] for a live key (stamping `last_used_at` best-effort), or `None`
/// when nothing matches. The key's `sub`/`role`/`email` come from its OWNING user's live row, so
/// a key that has no owner (or whose owner was deleted) does not authenticate — fail closed.
pub async fn resolve_api_key(db: &sqlx::PgPool, token: &str) -> Option<Claims> {
    let hash = hash_key(token);
    let row = sqlx::query(
        "SELECT k.id, k.tenant_id, k.created_by, u.role, u.email \
         FROM api_keys k JOIN users u ON u.id = k.created_by \
         WHERE k.key_hash = $1 AND k.revoked_at IS NULL",
    )
    .bind(&hash)
    .fetch_optional(db)
    .await
    .ok()??;

    let id: Uuid = row.get("id");
    let tenant_id: Uuid = row.get("tenant_id");
    let created_by: Uuid = row.get("created_by");
    let role: String = row.get("role");
    let email: String = row.get("email");

    // Best-effort usage stamp: a bookkeeping write must never deny a valid key.
    let _ = sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await;

    let now = chrono::Utc::now().timestamp() as usize;
    Some(Claims {
        sub: created_by,
        email,
        aid: tenant_id,
        role,
        pwd_minted: false,
        exp: now + API_KEY_TTL_SECS,
        iat: now,
        wsid: None,
    })
}

#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreatedApiKey {
    pub id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub token: String,
}

/// `GET /api/v1/api-keys` — this tenant's keys, plaintext never included.
pub async fn list_api_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let tenant_id = claims.aid;

    let rows = sqlx::query(
        "SELECT id, name, key_prefix, created_at, last_used_at, revoked_at, \
                (key_sealed IS NOT NULL) AS recoverable \
         FROM api_keys WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(tenant_id)
    .fetch_all(&state.pool)
    .await?;

    let keys: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id"),
                "name": r.get::<String, _>("name"),
                "key_prefix": r.get::<String, _>("key_prefix"),
                "created_at": r.get::<chrono::NaiveDateTime, _>("created_at"),
                "last_used_at": r.get::<Option<chrono::NaiveDateTime>, _>("last_used_at"),
                "revoked_at": r.get::<Option<chrono::NaiveDateTime>, _>("revoked_at"),
                // `false` = a key minted with no sealed copy; the plaintext is gone for good.
                "recoverable": r.get::<bool, _>("recoverable"),
            })
        })
        .collect();

    Ok(Json(json!({ "api_keys": keys })))
}

/// `POST /api/v1/api-keys` — mint a key. The plaintext is returned here AND stays viewable via
/// `reveal`, so "shown once" is never the customer's only chance.
///
/// One live key per workspace: a second mint is refused with 409 up front (the customer is told
/// to Regenerate instead), and the partial unique index `api_keys_one_live_per_tenant` is the
/// store-level backstop that makes the same rule hold under concurrency.
pub async fn create_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateApiKeyRequest>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let tenant_id = claims.aid;
    let created_by = claims.sub;

    let name = req
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "API key".to_string());

    let already: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM api_keys WHERE tenant_id = $1 AND revoked_at IS NULL)",
    )
    .bind(tenant_id)
    .fetch_one(&state.pool)
    .await?;
    if already {
        return Err(AppError::Conflict(
            "you already have a key — use Regenerate".to_string(),
        ));
    }

    let mut tx = state.pool.begin().await?;
    let minted = mint_on(&mut tx, tenant_id, Some(created_by), &name).await?;
    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(json!(CreatedApiKey {
            id: minted.id,
            name: minted.name,
            key_prefix: minted.prefix,
            token: minted.token,
        })),
    ))
}

#[derive(Debug, Deserialize)]
pub struct RevealKeyRequest {
    /// The caller's CURRENT password — a stolen session token must not be enough to read a live
    /// credential.
    pub password: String,
}

/// `POST /api/v1/api-keys/:id/reveal` — show the key again (repeatable, never show-once).
pub async fn reveal_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<RevealKeyRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let tenant_id = claims.aid;

    let row = sqlx::query(
        "SELECT key_prefix, key_sealed FROM api_keys \
         WHERE id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("API key not found".to_string()))?;

    let prefix: String = row.get("key_prefix");
    let sealed: Option<String> = row.get("key_sealed");

    // ── re-authenticate against the caller's CURRENT password ─────────────────────────────────
    let caller_id = claims.sub;
    let stored: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(caller_id)
            .fetch_optional(&state.pool)
            .await?;
    let stored = stored.ok_or_else(|| AppError::Unauthorized("User not found".to_string()))?;
    let ok = crate::auth::models::verify_password(&req.password, &stored)
        .map_err(|_| AppError::Unauthorized("Current password is incorrect".to_string()))?;
    if !ok {
        return Err(AppError::Unauthorized(
            "Current password is incorrect".to_string(),
        ));
    }

    let sealed = sealed.ok_or_else(|| {
        AppError::NotFound(
            "This key was created before keys were stored recoverably and cannot be displayed. \
             Regenerate it to get a key you can view and copy."
                .to_string(),
        )
    })?;
    let token = crate::security::provider_key_crypto::decrypt_from_storage(&state.pool, &sealed)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, key = %id, "API key reveal: sealed copy could not be opened");
            AppError::Internal(
                "This key could not be opened by the server's encryption key. Regenerate it."
                    .to_string(),
            )
        })?;
    if token.is_empty() {
        return Err(AppError::Internal(
            "This key could not be opened by the server's encryption key. Regenerate it."
                .to_string(),
        ));
    }

    // Audit row per reveal (best-effort; the general activity table is the audit store).
    if let Err(e) = sqlx::query(
        "INSERT INTO activity_log (id, tenant_id, user_id, action, entity_type, entity_id, metadata) \
         VALUES ($1, $2, $3, 'api_key_revealed', 'api_key', $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(caller_id)
    .bind(id)
    .bind(json!({ "key_prefix": prefix.clone() }))
    .execute(&state.pool)
    .await
    {
        tracing::error!(error = %e, key = %id, "API key reveal: audit row could not be written");
    }

    Ok(Json(
        json!({ "id": id.to_string(), "key_prefix": prefix, "token": token }),
    ))
}

/// `DELETE /api/v1/api-keys/:id` — soft-revoke (sets `revoked_at`). A revoked key stops
/// authenticating on the very next request.
pub async fn revoke_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let tenant_id = claims.aid;

    let res = sqlx::query(
        "UPDATE api_keys SET revoked_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(tenant_id)
    .execute(&state.pool)
    .await?;

    if res.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "API key not found or already revoked".to_string(),
        ));
    }

    if let Err(e) = sqlx::query(
        "INSERT INTO activity_log (id, tenant_id, user_id, action, entity_type, entity_id) \
         VALUES ($1, $2, $3, 'api_key_revoked', 'api_key', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(claims.sub)
    .bind(id)
    .execute(&state.pool)
    .await
    {
        tracing::error!(error = %e, key = %id, "activity_log insert failed for api_key_revoked");
    }

    Ok(Json(json!({ "message": "API key revoked", "id": id })))
}

/// `POST /api/v1/api-keys/:id/regenerate` — one atomic retire-then-issue: the old row is
/// revoked FIRST, then the replacement is minted, both in ONE transaction, so a failure at either
/// step leaves the customer with the key they already had, and a committed regenerate makes the
/// old key 401 on the very next request. Retire-before-mint is also what the partial unique index
/// `api_keys_one_live_per_tenant` requires: minting first would collide with the still-live old
/// key on (tenant_id) WHERE revoked_at IS NULL.
pub async fn regenerate_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let tenant_id = claims.aid;
    let created_by = claims.sub;

    let name: String = sqlx::query_scalar(
        "SELECT name FROM api_keys WHERE id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("API key not found".to_string()))?;

    let mut tx = state.pool.begin().await?;
    sqlx::query("UPDATE api_keys SET revoked_at = now() WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
    let minted = mint_on(&mut tx, tenant_id, Some(created_by), &name).await?;
    tx.commit().await?;

    tracing::info!(
        new_key = %minted.id,
        old_key = %id,
        key_prefix = %minted.prefix,
        "API key REGENERATED — the previous key no longer authenticates"
    );

    if let Err(e) = sqlx::query(
        "INSERT INTO activity_log (id, tenant_id, user_id, action, entity_type, entity_id, metadata) \
         VALUES ($1, $2, $3, 'api_key_regenerated', 'api_key', $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(created_by)
    .bind(minted.id)
    .bind(json!({ "old_key": id.to_string(), "key_prefix": minted.prefix.clone() }))
    .execute(&state.pool)
    .await
    {
        tracing::error!(error = %e, key = %minted.id, "activity_log insert failed for api_key_regenerated");
    }

    Ok((
        StatusCode::CREATED,
        Json(json!(CreatedApiKey {
            id: minted.id,
            name: minted.name,
            key_prefix: minted.prefix,
            token: minted.token,
        })),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_prefix_identifies_the_key_form() {
        let mut buf = [7u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        let token = format!("mcr_{}", hex::encode(buf));
        assert!(token.starts_with("mcr_"));
        // mcr_ + 64 hex chars.
        assert_eq!(token.len(), 68);
        assert_eq!(hash_key(&token).len(), 64);
        assert_eq!(token.chars().take(12).collect::<String>(), &token[..12]);
    }

    #[test]
    fn distinct_tokens_hash_differently_and_the_same_token_stably() {
        assert_eq!(hash_key("mcr_deadbeef"), hash_key("mcr_deadbeef"));
        assert_ne!(hash_key("mcr_deadbeef"), hash_key("mcr_deadbeee"));
    }

    #[test]
    fn a_key_shape_requires_material_after_the_prefix() {
        assert!(is_api_key("mcr_0123456789abcdef"));
        assert!(!is_api_key("mcr_"));
        assert!(!is_api_key("mcr_zzzz"));
        assert!(!is_api_key("not-a-key"));
        assert!(!is_api_key(""));
    }
}
