//! Email Templates handler — CRUD for email templates with admin auth.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

/// Full email template row.
///
/// Two fields used to disagree with the live table, and every read collapsed or
/// 500'd because of it:
///  - `aid` is NULLABLE (the live global template has aid = NULL, and the table's
///    own unique index is `(template_type, COALESCE(aid, nil), is_default)`).
///    Required as `Uuid` it failed with `unexpected null` on every row — which
///    `list` turned into `200 {"count":1,"items":[]}` and `get` into a 500.
///  - `is_html` is not a column of this app's `email_templates` table (verified
///    with `\d`); `#[sqlx(default)]` keeps a read from erroring on a column the
///    table never had, and still decodes the real value if the column is added.
///    The schema drift this reflects (POST/PUT also name `is_html`) is tracked
///    separately — it is a loud 500, not one of this change's silent swallows.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EmailTemplate {
    pub id: Uuid,
    pub aid: Option<Uuid>,
    pub name: String,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub html_body: Option<String>,
    #[sqlx(default)]
    pub is_html: Option<bool>,
    pub is_default: Option<bool>,
    pub template_type: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub template_type: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateInput {
    pub name: String,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub html_body: Option<String>,
    pub is_html: Option<bool>,
    pub is_default: Option<bool>,
    pub template_type: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateInput {
    pub name: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub html_body: Option<String>,
    pub is_html: Option<bool>,
    pub is_default: Option<bool>,
    pub template_type: Option<String>,
}

/// GET /api/v1/email-templates
pub async fn list(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, AppError> {
    let limit = query.limit.unwrap_or(50).min(100);
    let offset = query.offset.unwrap_or(0);

    let items = if let Some(tt) = &query.template_type {
        sqlx::query_as::<_, EmailTemplate>(
            "SELECT * FROM email_templates WHERE template_type = $1 ORDER BY name LIMIT $2 OFFSET $3"
        )
        .bind(tt).bind(limit).bind(offset)
        .fetch_all(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!("email_templates.list: filtered query failed (template_type={tt}): {e}");
            e
        })?
    } else {
        sqlx::query_as::<_, EmailTemplate>(
            "SELECT * FROM email_templates ORDER BY name LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!("email_templates.list: query failed: {e}");
            e
        })?
    };

    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM email_templates")
        .fetch_one(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!("email_templates.list: count query failed: {e}");
            e
        })?;

    Ok(Json(json!({ "items": items, "count": count })))
}

/// GET /api/v1/email-templates/{id}
pub async fn get(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let item = sqlx::query_as::<_, EmailTemplate>("SELECT * FROM email_templates WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Email template not found".to_string()))?;

    Ok(Json(json!({"item": item})))
}

/// POST /api/v1/email-templates
pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateInput>,
) -> Result<Json<Value>, AppError> {
    let id = Uuid::new_v4();
    let aid = Uuid::nil();

    sqlx::query(
        r#"INSERT INTO email_templates (id, aid, name, subject, body, html_body, is_html, is_default, template_type)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#
    )
    .bind(id).bind(aid)
    .bind(&body.name)
    .bind(&body.subject)
    .bind(&body.body)
    .bind(&body.html_body)
    .bind(body.is_html.unwrap_or(true))
    .bind(body.is_default.unwrap_or(false))
    .bind(&body.template_type)
    .execute(&state.pool).await?;

    let item = sqlx::query_as::<_, EmailTemplate>("SELECT * FROM email_templates WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;

    Ok(Json(json!({"item": item})))
}

/// PUT /api/v1/email-templates/{id}
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateInput>,
) -> Result<Json<Value>, AppError> {
    sqlx::query(
        r#"UPDATE email_templates SET
            name = COALESCE($1, name),
            subject = COALESCE($2, subject),
            body = COALESCE($3, body),
            html_body = COALESCE($4, html_body),
            is_html = COALESCE($5, is_html),
            is_default = COALESCE($6, is_default),
            template_type = COALESCE($7, template_type),
            updated_at = NOW()
           WHERE id = $8"#,
    )
    .bind(&body.name)
    .bind(&body.subject)
    .bind(&body.body)
    .bind(&body.html_body)
    .bind(body.is_html)
    .bind(body.is_default)
    .bind(&body.template_type)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, EmailTemplate>("SELECT * FROM email_templates WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;

    Ok(Json(json!({"item": item})))
}

/// DELETE /api/v1/email-templates/{id}
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    sqlx::query("DELETE FROM email_templates WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;

    Ok(Json(json!({"status": "deleted"})))
}
