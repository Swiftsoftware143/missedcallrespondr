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

/// Presence check for the columns `create` fills that are `NOT NULL` with no server default.
///
/// Returns the app's own error shape, `{"error": "<field> is required"}`, naming *every* field
/// the request left out — so a caller that omits two is told about both on the first call
/// instead of one per round trip. A single missing field keeps the exact wording already used
/// elsewhere in this codebase (auth/handlers.rs, portfolio_handler.rs).
///
/// An empty string is deliberately NOT rejected: it inserts fine today, and tightening it would
/// be a behaviour change on a request that currently succeeds.
fn required_create_fields(
    name: Option<String>,
    subject: Option<String>,
    template_type: Option<String>,
) -> Result<(String, String, String), AppError> {
    match (name, subject, template_type) {
        (Some(name), Some(subject), Some(template_type)) => Ok((name, subject, template_type)),
        (name, subject, template_type) => {
            let missing: Vec<&str> = [
                ("name", name.is_none()),
                ("subject", subject.is_none()),
                ("template_type", template_type.is_none()),
            ]
            .into_iter()
            .filter(|(_, is_missing)| *is_missing)
            .map(|(field, _)| field)
            .collect();

            Err(AppError::BadRequest(match missing.as_slice() {
                [one] => format!("{one} is required"),
                many => format!("{} are required", many.join(", ")),
            }))
        }
    }
}

/// The table's own rule, which BOTH write paths can violate:
/// `idx_email_templates_unique` is UNIQUE (template_type, COALESCE(aid,nil), is_default)
/// WHERE aid IS NULL AND is_default = true — at most one global default per `template_type`.
///
/// `create` mapped that to a 409 naming the field; `update` did not, so an admin using the SPA's
/// "set is_default" action on a second template of an already-defaulted type got the index error
/// back as a **500**. Both paths now answer the same 409 in the app's JSON shape
/// (`{"error": "a default template for template_type '<type>' already exists"}`).
fn default_conflict(template_type: &str) -> AppError {
    AppError::Conflict(format!(
        "a default template for template_type '{template_type}' already exists"
    ))
}

/// Is this error the one-default-per-type unique index firing?
fn is_default_conflict(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.constraint() == Some("idx_email_templates_unique"))
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
    let (name, subject, template_type) =
        required_create_fields(body.name, body.subject, body.template_type)?;

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
    .map_err(|e| {
        // The table says at most one global default per `template_type`. Answer that as a
        // Conflict that names the field rather than leaking the index error as a 500.
        if is_default_conflict(&e) {
            tracing::warn!(
                "email_templates.create: rejected a second global default (template_type={template_type})"
            );
            default_conflict(&template_type)
        } else {
            AppError::from(e)
        }
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
    let updated = sqlx::query(
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
    .await;

    // Setting `is_default = true` here can collide with the row that is already the default for
    // that `template_type` (the SPA's "set is_default" action is exactly that request). Answer it
    // the same way `create` does — 409 in the app's JSON shape — instead of returning the raw
    // index error as a 500. Matched out here, not in a `map_err` closure: the message needs the
    // row's own `template_type`, which requires an await.
    if let Err(e) = updated {
        if is_default_conflict(&e) {
            let existing: Option<String> = sqlx::query_scalar::<_, String>(
                "SELECT template_type FROM email_templates WHERE id = $1",
            )
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten();
            let template_type = existing
                .or_else(|| body.template_type.clone())
                .unwrap_or_else(|| "this type".to_string());
            tracing::warn!(
                "email_templates.update: rejected a second global default (id={id}, template_type={template_type})"
            );
            return Err(default_conflict(&template_type));
        }
        return Err(AppError::from(e));
    }

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
    use super::{default_conflict, required_create_fields};
    use crate::error::AppError;

    /// Both write paths answer the table's one-default-per-type rule the same way: a 409 whose
    /// body is the app's own JSON error naming the type. `update` used to return the raw index
    /// error as a 500 (card t_e0700e64, captured live before the fix).
    #[test]
    fn a_second_default_is_a_conflict_that_names_the_type() {
        match default_conflict("welcome") {
            AppError::Conflict(msg) => assert_eq!(
                msg,
                "a default template for template_type 'welcome' already exists"
            ),
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    fn err_of(name: Option<&str>, subject: Option<&str>, tt: Option<&str>) -> String {
        let opt = |v: Option<&str>| v.map(str::to_string);
        match required_create_fields(opt(name), opt(subject), opt(tt)) {
            Err(AppError::BadRequest(msg)) => msg,
            other => panic!("expected a 400 naming the fields, got {other:?}"),
        }
    }

    /// The defect (card t_1774cee3): `template_type` was bound as a NULL and reached Postgres,
    /// which answered a 500 carrying the raw SQL error. It must never get that far.
    #[test]
    fn an_absent_template_type_is_named_in_the_400() {
        assert_eq!(
            err_of(Some("n"), Some("s"), None),
            "template_type is required"
        );
    }

    /// `subject` is `NOT NULL` too and answered the identical 500; found while fixing the above.
    #[test]
    fn an_absent_subject_is_named_in_the_400() {
        assert_eq!(
            err_of(Some("n"), None, Some("welcome")),
            "subject is required"
        );
    }

    #[test]
    fn an_absent_name_is_named_in_the_400() {
        assert_eq!(err_of(None, Some("s"), Some("welcome")), "name is required");
    }

    /// Every missing field is reported at once, so the card's own repro body (`{"name": "..."}`)
    /// still names `template_type` rather than only `subject`.
    #[test]
    fn every_absent_field_is_reported_together() {
        assert_eq!(
            err_of(Some("n"), None, None),
            "subject, template_type are required"
        );
        let msg = err_of(None, None, None);
        for field in ["name", "subject", "template_type"] {
            assert!(msg.contains(field), "{field} not named in {msg:?}");
        }
    }

    #[test]
    fn present_values_pass_through_unchanged() {
        let got =
            required_create_fields(Some("n".into()), Some("s".into()), Some("welcome".into()));
        assert_eq!(
            got.unwrap(),
            ("n".to_string(), "s".to_string(), "welcome".to_string())
        );
        // An empty string inserts fine today; tightening it would change a request that
        // currently succeeds, so it must stay accepted.
        let empty = required_create_fields(Some(String::new()), Some("s".into()), Some("w".into()));
        assert_eq!(
            empty.unwrap(),
            (String::new(), "s".to_string(), "w".to_string())
        );
    }
}
