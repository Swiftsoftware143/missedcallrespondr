use axum::extract::{Path, State, Extension};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::config::Claims;
use crate::error::AppError;
use crate::state::AppState;

type ApiResult<T> = Result<T, AppError>;

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct ProviderKey {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub provider: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub metadata: Option<Value>,
    pub is_active: bool,
    pub scope: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
pub struct MaskedProviderKey {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub provider: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub metadata: Option<Value>,
    pub is_active: bool,
    pub scope: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpsertProviderKeyRequest {
    pub provider: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub metadata: Option<Value>,
    pub scope: Option<String>,
}

pub fn mask_key(key: &str) -> String {
    if key.len() <= 6 {
        return String::from("****");
    }
    let first3 = &key[..3];
    let last3 = &key[key.len()-3..];
    format!("{}...{}", first3, last3)
}

pub async fn list_provider_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Vec<MaskedProviderKey>>> {
    let tenant_id: Uuid = claims.tenant_id;

    let keys = sqlx::query_as::<_, ProviderKey>(
        "SELECT pk.id, pk.tenant_id, pk.provider, pk.api_key, pk.base_url, pk.metadata, pk.is_active, pk.scope, pk.created_at, pk.updated_at
         FROM provider_keys pk
         WHERE pk.tenant_id = $1
         ORDER BY pk.provider"
    )
    .bind(tenant_id)
    .fetch_all(&state.pool)
    .await?;

    let masked: Vec<MaskedProviderKey> = keys.into_iter().map(|k| {
        MaskedProviderKey {
            id: k.id,
            tenant_id: k.tenant_id,
            provider: k.provider,
            api_key: mask_key(&k.api_key),
            base_url: k.base_url,
            metadata: k.metadata,
            is_active: k.is_active,
            scope: k.scope,
            created_at: k.created_at,
            updated_at: k.updated_at,
        }
    }).collect();

    Ok(Json(masked))
}

pub async fn upsert_provider_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<UpsertProviderKeyRequest>,
) -> ApiResult<Json<Value>> {
    let tenant_id: Uuid = claims.tenant_id;

    // Verify provider exists
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM available_providers WHERE key = $1"
    )
    .bind(&req.provider)
    .fetch_one(&state.pool)
    .await?;

    if exists == 0 {
        return Err(AppError::BadRequest(format!("Unknown provider: {}", req.provider)));
    }

    // BYOK gate: when saving a telnyx key, check the tenant's plan allows BYOK
    if req.provider == "telnyx" {
        let plan_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT plan_id FROM tenant_plans WHERE tenant_id = $1 AND status = 'active'"
        )
        .bind(tenant_id)
        .fetch_optional(&state.pool)
        .await?
        .flatten();

        if let Some(pid) = plan_id {
            let byok_allowed: Option<i32> = sqlx::query_scalar(
                "SELECT limit_value FROM feature_limits WHERE plan_id = $1 AND feature_key = 'bring_your_own_key'"
            )
            .bind(pid)
            .fetch_optional(&state.pool)
            .await?
            .flatten();

            let allowed = match byok_allowed {
                Some(v) => v == -1 || v == 1,
                None => false,
            };

            if !allowed {
                return Err(AppError::BadRequest(
                    String::from("Bring Your Own Key is not available on your current plan. Upgrade to Pro or Enterprise to use your own Telnyx key.")
                ));
            }
        } else {
            return Err(AppError::BadRequest(
                String::from("You do not have an active plan. Select a plan before configuring BYOK.")
            ));
        }
    }

    let scope = req.scope.unwrap_or_else(|| String::from("tenant"));

    let result = sqlx::query_as::<_, ProviderKey>(
        "INSERT INTO provider_keys (tenant_id, provider, api_key, base_url, metadata, scope)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (tenant_id, provider)
         DO UPDATE SET api_key = EXCLUDED.api_key,
                       base_url = COALESCE(EXCLUDED.base_url, provider_keys.base_url),
                       metadata = COALESCE(EXCLUDED.metadata, provider_keys.metadata),
                       scope = EXCLUDED.scope,
                       updated_at = NOW()
         RETURNING *"
    )
    .bind(tenant_id)
    .bind(&req.provider)
    .bind(&req.api_key)
    .bind(&req.base_url)
    .bind(&req.metadata)
    .bind(&scope)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(json!({
        "id": result.id,
        "provider": result.provider,
        "api_key": mask_key(&result.api_key),
        "scope": result.scope,
        "is_active": result.is_active
    })))
}

pub async fn delete_provider_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(provider): Path<String>,
) -> ApiResult<Json<Value>> {
    let tenant_id: Uuid = claims.tenant_id;

    let result = sqlx::query(
        "DELETE FROM provider_keys WHERE tenant_id = $1 AND provider = $2"
    )
    .bind(tenant_id)
    .bind(&provider)
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        let err_msg = format!("Provider key not found");
        return Err(AppError::NotFound(err_msg));
    }

    Ok(Json(json!({"deleted": true, "provider": provider})))
}

/// No auth required - public endpoint to list available providers
pub async fn list_available_providers(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<Value>>> {
    let rows = sqlx::query(
        "SELECT key, name, description, requires_base_url, requires_metadata, icon
         FROM available_providers ORDER BY name "
    )
    .fetch_all(&state.pool)
    .await?;

    let mut providers: Vec<Value> = Vec::new();
    for row in rows.iter() {
        let key: String = row.get("key");
        let name: String = row.get("name");
        let desc: Option<String> = row.get("description");
        let base_url: bool = row.get("requires_base_url");
        let req_meta: Value = row.get("requires_metadata");
        let icon_val: Option<String> = row.get("icon");
        providers.push(Value::Object(serde_json::Map::from_iter([
            (String::from("key"), Value::String(key)),
            (String::from("name"), Value::String(name)),
            (String::from("description"), Value::from(desc)),
            (String::from("requires_base_url"), Value::Bool(base_url)),
            (String::from("requires_metadata"), Value::from(req_meta)),
            (String::from("icon"), Value::from(icon_val)),
        ])));
    }

    Ok(Json(providers))
}
