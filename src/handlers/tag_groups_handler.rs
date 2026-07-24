//! Tag Groups handler — full tenant-scoped CRUD

use axum::{
    extract::{Extension, Path, State},
    Json,
};
use serde_json::json;
use uuid::Uuid;
use sqlx::Row;

use crate::{config::Claims, error::AppError, state::AppState};

/// GET /api/v1/tag-groups — returns groups with tag count
pub async fn list(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<serde_json::Value>>, AppError> {
    let rows = sqlx::query(
        r#"
        SELECT
            tg.id,
            tg.tenant_id,
            tg.name,
            tg.color,
            tg.sort_order,
            tg.created_at,
            tg.updated_at,
            (SELECT COUNT(*) FROM tags t WHERE t.group_id = tg.id) AS tag_count
        FROM tag_groups tg
        WHERE tg.tenant_id = $1
        ORDER BY tg.sort_order, tg.name
        "#
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;

    let groups: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
            "tenant_id": r.try_get::<Uuid,_>("tenant_id").map(|u| u.to_string()).unwrap_or_default(),
            "name": r.try_get::<String,_>("name").unwrap_or_default(),
            "color": r.try_get::<String,_>("color").unwrap_or_else(|_| "#6366f1".into()),
            "sort_order": r.try_get::<i32,_>("sort_order").unwrap_or(0),
            "tag_count": r.try_get::<i64,_>("tag_count").unwrap_or(0),
            "created_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("created_at")
                .map(|d| d.to_rfc3339()).unwrap_or_default(),
            "updated_at": r.try_get::<chrono::DateTime<chrono::Utc>,_>("updated_at")
                .map(|d| d.to_rfc3339()).unwrap_or_default(),
        })
    }).collect();

    Ok(Json(groups))
}

/// POST /api/v1/tag-groups
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

    let sort_order = body.get("sort_order")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;

    let id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO tag_groups (id, tenant_id, name, color, sort_order) VALUES ($1, $2, $3, $4, $5)"
    )
    .bind(id)
    .bind(claims.aid)
    .bind(name)
    .bind(color)
    .bind(sort_order)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({
        "id": id.to_string(),
        "tenant_id": claims.aid.to_string(),
        "name": name,
        "color": color,
        "sort_order": sort_order,
        "tag_count": 0,
    })))
}

/// GET /api/v1/tag-groups/:id
pub async fn get(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let row = sqlx::query(
        r#"
        SELECT
            tg.id,
            tg.tenant_id,
            tg.name,
            tg.color,
            tg.sort_order,
            tg.created_at,
            tg.updated_at,
            (SELECT COUNT(*) FROM tags t WHERE t.group_id = tg.id) AS tag_count
        FROM tag_groups tg
        WHERE tg.id = $1 AND tg.tenant_id = $2
        "#
    )
    .bind(id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Tag group not found".into()))?;

    Ok(Json(json!({
        "id": row.try_get::<Uuid,_>("id").map(|u| u.to_string()).unwrap_or_default(),
        "tenant_id": row.try_get::<Uuid,_>("tenant_id").map(|u| u.to_string()).unwrap_or_default(),
        "name": row.try_get::<String,_>("name").unwrap_or_default(),
        "color": row.try_get::<String,_>("color").unwrap_or_else(|_| "#6366f1".into()),
        "sort_order": row.try_get::<i32,_>("sort_order").unwrap_or(0),
        "tag_count": row.try_get::<i64,_>("tag_count").unwrap_or(0),
        "created_at": row.try_get::<chrono::DateTime<chrono::Utc>,_>("created_at")
            .map(|d| d.to_rfc3339()).unwrap_or_default(),
        "updated_at": row.try_get::<chrono::DateTime<chrono::Utc>,_>("updated_at")
            .map(|d| d.to_rfc3339()).unwrap_or_default(),
    })))
}

/// PUT /api/v1/tag-groups/:id
pub async fn update(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Verify ownership
    let _existing = sqlx::query("SELECT id FROM tag_groups WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Tag group not found".into()))?;

    let name = body.get("name").and_then(|v| v.as_str());
    let color = body.get("color").and_then(|v| v.as_str());
    let sort_order = body.get("sort_order").and_then(|v| v.as_i64());

    sqlx::query(
        r#"
        UPDATE tag_groups SET
            name = COALESCE(NULLIF($1, ''), name),
            color = COALESCE(NULLIF($2, ''), color),
            sort_order = COALESCE($3, sort_order),
            updated_at = NOW()
        WHERE id = $4 AND tenant_id = $5
        "#
    )
    .bind(name)
    .bind(color)
    .bind(sort_order.map(|v| v as i32))
    .bind(id)
    .bind(claims.aid)
    .execute(&state.pool)
    .await?;

    // Return updated record
    let row = sqlx::query(
        r#"
        SELECT id, tenant_id, name, color, sort_order,
            (SELECT COUNT(*) FROM tags t WHERE t.group_id = tag_groups.id) AS tag_count
        FROM tag_groups WHERE id = $1
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
        "sort_order": row.try_get::<i32,_>("sort_order").unwrap_or(0),
        "tag_count": row.try_get::<i64,_>("tag_count").unwrap_or(0),
    })))
}

/// DELETE /api/v1/tag-groups/:id
pub async fn delete(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Set group_id = NULL on tags in this group first (ON DELETE SET NULL handles this,
    // but we do it explicitly for clarity and to ensure tag counts are correct)
    sqlx::query("UPDATE tags SET group_id = NULL, updated_at = NOW() WHERE group_id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;

    let result = sqlx::query("DELETE FROM tag_groups WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Tag group not found".into()));
    }

    Ok(Json(json!({"message": "Tag group deleted"})))
}
