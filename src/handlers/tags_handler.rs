//! Tags handler — full tenant-scoped CRUD with JOIN to tag_groups

use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;
use sqlx::Row;

use crate::{config::Claims, error::AppError, state::AppState};

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub search: Option<String>,
    pub group_id: Option<String>,
}

/// GET /api/v1/tags — tenant-scoped, JOIN with tag_groups, supports ?group_id= filter
pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let limit = query.limit.unwrap_or(50).min(200);
    let offset = query.offset.unwrap_or(0);

    let rows = if let Some(ref group_id_str) = query.group_id {
        let group_id = Uuid::parse_str(group_id_str)
            .map_err(|_| AppError::BadRequest("Invalid group_id".into()))?;
        sqlx::query(
            r#"
            SELECT
                t.id,
                t.tenant_id,
                t.name,
                t.color,
                t.group_id,
                t.sync_to_core,
                t.created_at,
                t.updated_at,
                tg.name AS group_name,
                tg.color AS group_color
            FROM tags t
            LEFT JOIN tag_groups tg ON t.group_id = tg.id
            WHERE t.tenant_id = $1 AND t.group_id = $2
            ORDER BY t.name
            LIMIT $3 OFFSET $4
            "#
        )
        .bind(&claims.aid)
        .bind(group_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else if let Some(ref search) = query.search {
        let pattern = format!("%{}%", search);
        sqlx::query(
            r#"
            SELECT
                t.id,
                t.tenant_id,
                t.name,
                t.color,
                t.group_id,
                t.sync_to_core,
                t.created_at,
                t.updated_at,
                tg.name AS group_name,
                tg.color AS group_color
            FROM tags t
            LEFT JOIN tag_groups tg ON t.group_id = tg.id
            WHERE t.tenant_id = $1 AND t.name ILIKE $2
            ORDER BY t.name
            LIMIT $3 OFFSET $4
            "#
        )
        .bind(&claims.aid)
        .bind(&pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query(
            r#"
            SELECT
                t.id,
                t.tenant_id,
                t.name,
                t.color,
                t.group_id,
                t.sync_to_core,
                t.created_at,
                t.updated_at,
                tg.name AS group_name,
                tg.color AS group_color
            FROM tags t
            LEFT JOIN tag_groups tg ON t.group_id = tg.id
            WHERE t.tenant_id = $1
            ORDER BY t.name
            LIMIT $2 OFFSET $3
            "#
        )
        .bind(&claims.aid)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await?
    };

    let tags: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "tenant_id": r.try_get::<Uuid,_>("tenant_id").map(|u| u.to_string()).unwrap_or_default(),
            "name": r.try_get::<String,_>("name").unwrap_or_default(),
            "color": r.try_get::<String,_>("color").unwrap_or_else(|_| "#6366f1".into()),
            "group_id": r.try_get::<Option<Uuid>,_>("group_id").ok().flatten().map(|u| u.to_string()),
            "sync_to_core": r.try_get::<bool,_>("sync_to_core").unwrap_or(true),
            "group_name": r.try_get::<Option<String>,_>("group_name").ok().flatten().unwrap_or_default(),
            "group_color": r.try_get::<Option<String>,_>("group_color").ok().flatten().unwrap_or_default(),
            "created_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("created_at")
                .map(|d| d.to_rfc3339()).unwrap_or_default(),
            "updated_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("updated_at")
                .map(|d| d.to_rfc3339()).unwrap_or_default(),
        })
    }).collect();

    Ok(Json(tags))
}

/// POST /api/v1/tags
pub async fn create(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let name = body.get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("name is required".into()))?;

    let color = body.get("color")
        .and_then(|v| v.as_str())
        .unwrap_or("#6366f1");

    let group_id: Option<Uuid> = body.get("group_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());

    let sync_to_core = body.get("sync_to_core")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO tags (id, tenant_id, name, color, group_id, sync_to_core) VALUES ($1, $2, $3, $4, $5, $6)"
    )
    .bind(id)
    .bind(&claims.aid)
    .bind(name)
    .bind(color)
    .bind(group_id)
    .bind(sync_to_core)
    .execute(&state.pool)
    .await?;

    // Fetch group name if group_id was provided
    let group_name = if let Some(gid) = group_id {
        sqlx::query_scalar::<_, Option<String>>("SELECT name FROM tag_groups WHERE id = $1")
            .bind(gid)
            .fetch_optional(&state.pool)
            .await?
            .flatten()
            .unwrap_or_default()
    } else {
        String::new()
    };

    Ok(Json(json!({
        "id": id.to_string(),
        "tenant_id": claims.aid.to_string(),
        "name": name,
        "color": color,
        "group_id": group_id.map(|u| u.to_string()),
        "sync_to_core": sync_to_core,
        "group_name": group_name,
        "group_color": "",
    })))
}

/// GET /api/v1/tags/:id
pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row = sqlx::query(
        r#"
        SELECT
            t.id,
            t.tenant_id,
            t.name,
            t.color,
            t.group_id,
            t.sync_to_core,
            t.created_at,
            t.updated_at,
            tg.name AS group_name,
            tg.color AS group_color
        FROM tags t
        LEFT JOIN tag_groups tg ON t.group_id = tg.id
        WHERE t.id = $1 AND t.tenant_id = $2
        "#
    )
    .bind(id)
    .bind(&claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Tag not found".into()))?;

    Ok(Json(json!({
        "id": row.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
        "tenant_id": row.try_get::<Uuid,_>("tenant_id").map(|u| u.to_string()).unwrap_or_default(),
        "name": row.try_get::<String,_>("name").unwrap_or_default(),
        "color": row.try_get::<String,_>("color").unwrap_or_else(|_| "#6366f1".into()),
        "group_id": row.try_get::<Option<Uuid>,_>("group_id").ok().flatten().map(|u| u.to_string()),
        "sync_to_core": row.try_get::<bool,_>("sync_to_core").unwrap_or(true),
        "group_name": row.try_get::<Option<String>,_>("group_name").ok().flatten().unwrap_or_default(),
        "group_color": row.try_get::<Option<String>,_>("group_color").ok().flatten().unwrap_or_default(),
        "created_at": row.try_get::<chrono::DateTime<chrono::Utc>,_>("created_at")
            .map(|d| d.to_rfc3339()).unwrap_or_default(),
        "updated_at": row.try_get::<chrono::DateTime<chrono::Utc>,_>("updated_at")
            .map(|d| d.to_rfc3339()).unwrap_or_default(),
    })))
}

/// PUT /api/v1/tags/:id
pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Verify ownership
    let _existing = sqlx::query("SELECT id FROM tags WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(&claims.aid)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Tag not found".into()))?;

    let name = body.get("name").and_then(|v| v.as_str());
    let color = body.get("color").and_then(|v| v.as_str());
    let sync_to_core = body.get("sync_to_core").and_then(|v| v.as_bool());
    let group_id: Option<Option<Uuid>> = match body.get("group_id") {
        Some(v) if v.is_null() => Some(None),
        Some(v) => v.as_str().and_then(|s| Uuid::parse_str(s).ok()).map(Some),
        None => None,
    };

    sqlx::query(
        r#"
        UPDATE tags SET
            name = COALESCE(NULLIF($1, ''), name),
            color = COALESCE(NULLIF($2, ''), color),
            sync_to_core = COALESCE($3, sync_to_core),
            group_id = CASE WHEN $4::uuid IS NULL AND $5::boolean THEN NULL ELSE COALESCE($4::uuid, group_id) END,
            updated_at = NOW()
        WHERE id = $6 AND tenant_id = $7
        "#
    )
    .bind(name)
    .bind(color)
    .bind(sync_to_core)
    .bind(group_id.flatten())
    .bind(group_id.is_some()) // indicates whether group_id was explicitly provided (even as null)
    .bind(id)
    .bind(&claims.aid)
    .execute(&state.pool)
    .await?;

    // Return updated record
    let row = sqlx::query(
        r#"
        SELECT
            t.id, t.tenant_id, t.name, t.color, t.group_id, t.sync_to_core,
            tg.name AS group_name, tg.color AS group_color
        FROM tags t
        LEFT JOIN tag_groups tg ON t.group_id = tg.id
        WHERE t.id = $1
        "#
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(json!({
        "id": row.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
        "tenant_id": row.try_get::<Uuid,_>("tenant_id").map(|u| u.to_string()).unwrap_or_default(),
        "name": row.try_get::<String,_>("name").unwrap_or_default(),
        "color": row.try_get::<String,_>("color").unwrap_or_else(|_| "#6366f1".into()),
        "group_id": row.try_get::<Option<Uuid>,_>("group_id").ok().flatten().map(|u| u.to_string()),
        "sync_to_core": row.try_get::<bool,_>("sync_to_core").unwrap_or(true),
        "group_name": row.try_get::<Option<String>,_>("group_name").ok().flatten().unwrap_or_default(),
        "group_color": row.try_get::<Option<String>,_>("group_color").ok().flatten().unwrap_or_default(),
    })))
}

/// DELETE /api/v1/tags/:id
pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM tags WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(&claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Tag not found".into()));
    }

    Ok(Json(json!({"message": "Tag deleted"})))
}
