use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
    Argon2,
};
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    Json,
};
use rand::Rng;
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::config::Claims;
use crate::error::AppError;
use crate::features;
use crate::state::AppState;
use crate::validation::{check_len, check_opt_len, max};

/// Gate rule 5d (class 14): a statement must be a COMPLETE compile-time literal, never assembled at
/// run time. `update_api_key` has three optional fields, so the UPDATE has exactly 2^3 = 8 texts;
/// all eight are const literals and only the CHOICE between them is run time. The values are bind
/// parameters, which also retires the hand-rolled quote escaping (`name = '{}'` +
/// `.replace('\'', "''")`) this statement used to carry.
const UPD_NAME_URL_ACTIVE: &str = "UPDATE api_keys SET name = $1, target_url = $2, is_active = $3, updated_at = NOW() WHERE id = $4";
const UPD_NAME_URL: &str =
    "UPDATE api_keys SET name = $1, target_url = $2, updated_at = NOW() WHERE id = $3";
const UPD_NAME_ACTIVE: &str =
    "UPDATE api_keys SET name = $1, is_active = $2, updated_at = NOW() WHERE id = $3";
const UPD_URL_ACTIVE: &str =
    "UPDATE api_keys SET target_url = $1, is_active = $2, updated_at = NOW() WHERE id = $3";
const UPD_NAME: &str = "UPDATE api_keys SET name = $1, updated_at = NOW() WHERE id = $2";
const UPD_URL: &str = "UPDATE api_keys SET target_url = $1, updated_at = NOW() WHERE id = $2";
const UPD_ACTIVE: &str = "UPDATE api_keys SET is_active = $1, updated_at = NOW() WHERE id = $2";
const UPD_TIME: &str = "UPDATE api_keys SET updated_at = NOW() WHERE id = $1";

/// The one place the eight literals are chosen. Separate from the handler so the test below can
/// assert the choice AND the placeholder/bind order for all eight flag combinations.
fn update_api_key_sql(has_name: bool, has_url: bool, has_active: bool) -> &'static str {
    match (has_name, has_url, has_active) {
        (true, true, true) => UPD_NAME_URL_ACTIVE,
        (true, true, false) => UPD_NAME_URL,
        (true, false, true) => UPD_NAME_ACTIVE,
        (false, true, true) => UPD_URL_ACTIVE,
        (true, false, false) => UPD_NAME,
        (false, true, false) => UPD_URL,
        (false, false, true) => UPD_ACTIVE,
        (false, false, false) => UPD_TIME,
    }
}

/// The stored key prefix, bound to `api_keys.prefix VARCHAR(8)`.
///
/// THE SAME CLASS, SERVER-SIDE. This constant used to be `"missedca_"` — NINE characters into a
/// `VARCHAR(8)` column — so EVERY `POST /api/v1/api-keys` answered `500 {"error":"Database error"}`
/// and stored nothing (measured live 2026-10-02 by the t_dd7be032 census: `value too long for type
/// character varying(8)` twice, one per attempt; `api_keys` held 0 rows before and after). The
/// census found it because the bounded-column sweep reaches server-written columns too; the client
/// `name` bound could not be proven either way while the route could not store a row at all.
/// `api_keys` is EMPTY live and the prefix is only stored and echoed (never used for lookup), so
/// making it fit breaks no existing key.
const API_KEY_PREFIX: &str = "missedca";

/// Compile-time proof that the constant fits `api_keys.prefix VARCHAR(8)` / `max::API_KEYS_PREFIX`:
/// a longer literal fails the BUILD, not a live request. (This is also what keeps
/// `max::API_KEYS_PREFIX` load-bearing in the non-test build.)
const _: () = assert!(API_KEY_PREFIX.len() <= crate::validation::max::API_KEYS_PREFIX);

fn generate_api_key() -> (String, String) {
    let prefix = API_KEY_PREFIX.to_string();
    let random_part: String = (0..16)
        .map(|_| format!("{:x}", rand::thread_rng().gen_range(0..16)))
        .collect();
    let raw_key = format!("missedcallrespondr_{}", random_part);
    (raw_key, prefix)
}

/// The prefix must fit `api_keys.prefix` — the 500 this pins was invisible until a probe asked for
/// the row back (kanban t_dd7be032).
#[cfg(test)]
mod prefix_bound_tests {
    use super::API_KEY_PREFIX;
    use crate::validation::max;

    #[test]
    fn the_stored_prefix_fits_its_column() {
        assert_eq!(API_KEY_PREFIX, "missedca");
        assert_eq!(API_KEY_PREFIX.chars().count(), max::API_KEYS_PREFIX);
        assert!(API_KEY_PREFIX.chars().count() <= max::API_KEYS_PREFIX);
    }
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
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    check_len("name", name, max::API_KEYS_NAME)?;
    // The plan gate runs AFTER the request is well-formed (kanban t_dd7be032): a name that breaks
    // `api_keys.name` is the caller's error and must not pay for a statement first.
    features::enforce_feature_limit(&state.pool, tenant_id, "max_api_keys", "Api Keys").await?;
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

    let keys: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.try_get::<&str, _>("id").unwrap_or(""),
                "name": row.try_get::<&str, _>("name").unwrap_or(""),
                "prefix": row.try_get::<&str, _>("prefix").unwrap_or(""),
                "target_url": row.try_get::<Option<&str>, _>("target_url").unwrap_or(None),
                "is_active": row.try_get::<bool, _>("is_active").unwrap_or(false),
            })
        })
        .collect();

    Ok(Json(json!({"api_keys": keys})))
}

pub async fn update_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let name = req.get("name").and_then(|v| v.as_str());
    let url = req.get("target_url").and_then(|v| v.as_str());
    let active = req.get("is_active").and_then(|v| v.as_bool());
    check_opt_len("name", name, max::API_KEYS_NAME)?;

    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM api_keys WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_one(&state.pool)
    .await?;

    if existing == 0 {
        return Err(AppError::NotFound("API key not found".into()));
    }

    let sql = update_api_key_sql(name.is_some(), url.is_some(), active.is_some());

    // Binds in the order their placeholders appear in every one of the eight literals:
    // name, target_url, is_active, then the row id.
    let mut q = sqlx::query(sql);
    if let Some(v) = name {
        q = q.bind(v);
    }
    if let Some(v) = url {
        q = q.bind(v);
    }
    if let Some(v) = active {
        q = q.bind(v);
    }
    q = q.bind(id);
    q.execute(&state.pool).await?;

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

#[cfg(test)]
mod update_sql_tests {
    //! Gate rule 5d (class 14) pin for `update_api_key` (kanban t_e7903ef6). Each literal must
    //! select exactly the columns the caller provided plus the always-present `updated_at`, and the
    //! placeholders must be `$1..$N` in the same order the handler binds them: name, target_url,
    //! is_active, then the id. The targets are read out of each statement's own text, so the
    //! assertion is about the statement and not about a constant equalling itself.
    use super::*;

    const PLACEHOLDERS: [&str; 4] = ["$1", "$2", "$3", "$4"];

    fn set_targets(sql: &str) -> Vec<&str> {
        let set_clause = sql
            .split(" SET ")
            .nth(1)
            .expect("statement has a SET clause")
            .split(" WHERE ")
            .next()
            .expect("SET comes before WHERE");
        set_clause
            .split(", ")
            .map(|col| col.split(" = ").next().unwrap_or(col))
            .collect()
    }

    #[test]
    fn every_flag_combination_selects_the_matching_columns_and_bind_order() {
        for has_name in [false, true] {
            for has_url in [false, true] {
                for has_active in [false, true] {
                    let sql = update_api_key_sql(has_name, has_url, has_active);
                    let mut want: Vec<&str> = Vec::new();
                    if has_name {
                        want.push("name");
                    }
                    if has_url {
                        want.push("target_url");
                    }
                    if has_active {
                        want.push("is_active");
                    }
                    want.push("updated_at");
                    assert_eq!(
                        set_targets(sql),
                        want,
                        "SET targets for ({has_name}, {has_url}, {has_active})"
                    );
                    // one placeholder per column above, numbered $1..$N with no gaps
                    assert_eq!(sql.matches('$').count(), want.len());
                    assert!(PLACEHOLDERS[..want.len()].iter().all(|p| sql.contains(*p)));
                    // the row id is the last placeholder of every literal
                    assert!(sql.ends_with(PLACEHOLDERS[want.len() - 1]));
                }
            }
        }
    }
}
