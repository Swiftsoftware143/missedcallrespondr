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
/// One field used to disagree with the live table, and every read collapsed or
/// 500'd because of it:
///  - `aid` is NULLABLE (the live global template has aid = NULL, and the table's
///    own unique index is `(template_type, COALESCE(aid, nil), is_default)`).
///    Required as `Uuid` it failed with `unexpected null` on every row — which
///    `list` turned into `200 {"count":1,"items":[]}` and `get` into a 500.
///  - `is_html` was NOT a column of this app's `email_templates` table (verified
///    with `\d`), while `create`/`update` below and the templated-email lookup in
///    src/email.rs all named it: POST/PUT answered 500 `column "is_html" of
///    relation "email_templates" does not exist` and the lookup failed silently.
///    Migration 000017 (card t_99365fd5) adds the column and makes this repo the
///    owner of the table shape, so the real value now decodes; `#[sqlx(default)]`
///    stays as belt-and-braces for the read side.
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
    /// `name`, `subject` and `template_type` are all `NOT NULL` with no server default in
    /// the live table, so they are `Option` here only to tell "absent" apart from "present"
    /// and answer a 400 that names the field — see `create` below. `deserialize` itself
    /// must not reject them, or a missing field would 422 out of the `Json` extractor with
    /// a plain-text body instead of this app's `{"error": "…"}` shape.
    pub name: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub html_body: Option<String>,
    pub is_html: Option<bool>,
    pub is_default: Option<bool>,
    pub template_type: Option<String>,
}

/// Deliberately still all-`Option`: this handler is a COALESCE partial update, so an absent
/// field means "leave it alone". `www-admin/index.html` drives it as raw JSON (`"json": true`)
/// so the caller controls exactly which keys are present. Only `create` has to reject absence.
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

/// Presence check for a column that is `NOT NULL` with no server default.
///
/// Returns the app's own 400 shape, `{"error": "<field> is required"}`, matching the wording
/// already used elsewhere in this codebase (auth/handlers.rs, portfolio_handler.rs). An empty
/// string is deliberately NOT rejected: it inserts fine today, and tightening that would be a
/// behaviour change on a request that currently succeeds.
fn required_field(field: &str, value: Option<String>) -> Result<String, AppError> {
    value.ok_or_else(|| AppError::BadRequest(format!("{field} is required")))
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
///
/// The columns this handler fills from the request — `name`, `subject`, `template_type` —
/// are `NOT NULL` with no server default, but were bound straight through as `Option<String>`.
/// A request that omitted one therefore reached Postgres and came back as a **500 carrying the
/// raw SQL error**, e.g. (card t_1774cee3, reproduced live):
///   `null value in column "template_type" of relation "email_templates" violates not-null constraint`
/// `subject` failed identically and was found while fixing that. All three are now checked here
/// and answered as 400 naming the missing field — the contract `www-admin/index.html` already
/// sends (`Create template` = name, subject, body, html_body, template_type).
///
/// `aid` was bound as `Uuid::nil()`, which nothing in this app means: the live global default
/// row has `aid IS NULL`, and the table's own index is
/// `UNIQUE (template_type, COALESCE(aid, nil), is_default) WHERE aid IS NULL AND is_default = true`.
/// This route carries no tenant context, so `aid` can only mean "global" — and a nil-uuid row
/// was both invisible to `lookup_db_template`'s `aid = $2` and outside that index, which is how
/// a second global default for one `template_type` could be inserted silently. `NULL` puts the
/// row back inside the index, so a real duplicate is now a 409 that names the field.
pub async fn create(
    State(state): State<AppState>,
    Json(body): Json<CreateInput>,
) -> Result<Json<Value>, AppError> {
    let name = required_field("name", body.name)?;
    let subject = required_field("subject", body.subject)?;
    let template_type = required_field("template_type", body.template_type)?;

    let id = Uuid::new_v4();
    // Global template. See the doc comment: the live default row is `aid IS NULL`, and the
    // partial unique index only covers `aid IS NULL`.
    let aid: Option<Uuid> = None;

    sqlx::query(
        r#"INSERT INTO email_templates (id, aid, name, subject, body, html_body, is_html, is_default, template_type)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#
    )
    .bind(id).bind(aid)
    .bind(&name)
    .bind(&subject)
    .bind(&body.body)
    .bind(&body.html_body)
    .bind(body.is_html.unwrap_or(true))
    .bind(body.is_default.unwrap_or(false))
    .bind(&template_type)
    .execute(&state.pool)
    .await
    .map_err(|e| match &e {
        // The table says at most one global default per `template_type`. Answer that as a
        // Conflict that names the field rather than leaking the index error as a 500.
        sqlx::Error::Database(db) if db.constraint() == Some("idx_email_templates_unique") => {
            tracing::warn!(
                "email_templates.create: rejected a second global default (template_type={template_type})"
            );
            AppError::Conflict(format!(
                "a default template for template_type '{template_type}' already exists"
            ))
        }
        _ => AppError::from(e),
    })?;

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

    Ok(Json(json!({ "status": "deleted" })))
}

#[cfg(test)]
mod tests {
    use super::required_field;
    use crate::error::AppError;

    /// Every `NOT NULL`-without-default column `create` fills must be refused before the
    /// statement is built, naming the field, so Postgres never sees the NULL. Card t_1774cee3:
    /// `template_type` used to reach the database and answer 500 with the raw SQL error.
    #[test]
    fn absent_not_null_fields_are_named_in_a_400() {
        for field in ["name", "subject", "template_type"] {
            match required_field(field, None) {
                Err(AppError::BadRequest(msg)) => {
                    assert_eq!(
                        msg,
                        format!("{field} is required"),
                        "field not named: {msg}"
                    );
                }
                other => panic!("expected BadRequest for a missing {field}, got {other:?}"),
            }
        }
    }

    #[test]
    fn present_values_pass_through_unchanged() {
        assert_eq!(
            required_field("template_type", Some("welcome".into())).unwrap(),
            "welcome"
        );
        // An empty string inserts fine today; tightening it would change a request that
        // currently succeeds, so it must stay accepted.
        assert_eq!(required_field("name", Some(String::new())).unwrap(), "");
    }
}
