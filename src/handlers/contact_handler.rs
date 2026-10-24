use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use std::collections::HashMap;
use uuid::Uuid;

use crate::{
    config::Claims,
    error::AppError,
    features,
    models::contact::{Contact, CreateContactRequest, UpdateContactRequest},
    state::AppState,
};

// ---- contact field limits ------------------------------------------------------------------------
// Nothing in this path bounded a contact's strings before: the columns are `text`, the dashboard
// prints `name` verbatim in its "Recent activity" feed, and a 250-character name was accepted live
// (measured 2026-10-24: POST /api/v1/contacts answered 200 and stored it). One such row turned the
// operator's activity list into a single 250-character run of letters. The cap belongs at the WRITE
// (a well-formed request naming an impossible value), so the refusals are 422 with a sentence the
// contact form already displays; the dashboard clips as well, because rows written before this
// validation still exist and must not be able to stretch the panel either.
pub const MAX_CONTACT_NAME: usize = 120;
pub const MAX_CONTACT_PHONE: usize = 40;
pub const MAX_CONTACT_EMAIL: usize = 254;
pub const MAX_CONTACT_COMPANY: usize = 120;
pub const MAX_CONTACT_NOTES: usize = 2000;
pub const MAX_CONTACT_TAGS: usize = 20;
pub const MAX_CONTACT_TAG: usize = 40;

/// `Some(422)` when `value` is longer than `max` CHARACTERS (not bytes — a name in a non-Latin
/// script must not be rejected for a length it does not have).
fn too_long(field: &str, value: &str, max: usize) -> Option<AppError> {
    let n = value.chars().count();
    if n > max {
        Some(AppError::Unprocessable(format!(
            "{field} is {n} characters long; the limit is {max}. Shorten it and try again."
        )))
    } else {
        None
    }
}

/// Validate the contact fields a request actually carries. `None` means "the caller did not send
/// this field" (an update that leaves a value alone), so a row written before these limits cannot
/// block an unrelated edit.
fn validate_contact(
    name: Option<&str>,
    phone: Option<&str>,
    email: Option<&str>,
    company: Option<&str>,
    notes: Option<&str>,
    tags: Option<&Vec<String>>,
) -> Result<(), AppError> {
    if let Some(name) = name {
        if name.trim().is_empty() {
            return Err(AppError::Unprocessable(
                "A contact needs a name — type one and try again.".into(),
            ));
        }
        if let Some(e) = too_long("The contact name", name, MAX_CONTACT_NAME) {
            return Err(e);
        }
    }
    if let Some(phone) = phone {
        if let Some(e) = too_long("The phone number", phone.trim(), MAX_CONTACT_PHONE) {
            return Err(e);
        }
    }
    if let Some(email) = email {
        let trimmed = email.trim();
        if !trimmed.is_empty() {
            if let Some(e) = too_long("The email address", trimmed, MAX_CONTACT_EMAIL) {
                return Err(e);
            }
            if !trimmed.contains('@') || trimmed.contains(char::is_whitespace) {
                return Err(AppError::Unprocessable(
                    "That email address is not valid — it needs an @ and no spaces.".into(),
                ));
            }
        }
    }
    if let Some(company) = company {
        if let Some(e) = too_long("The company name", company, MAX_CONTACT_COMPANY) {
            return Err(e);
        }
    }
    if let Some(notes) = notes {
        if let Some(e) = too_long("The notes", notes, MAX_CONTACT_NOTES) {
            return Err(e);
        }
    }
    if let Some(tags) = tags {
        if tags.len() > MAX_CONTACT_TAGS {
            return Err(AppError::Unprocessable(format!(
                "{} tags is more than the {MAX_CONTACT_TAGS} a contact can carry. Remove some and try again.",
                tags.len()
            )));
        }
        for tag in tags {
            if let Some(e) = too_long("A tag", tag, MAX_CONTACT_TAG) {
                return Err(e);
            }
        }
    }
    Ok(())
}

pub async fn list_contacts(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<Contact>>, AppError> {
    let items = sqlx::query_as::<_, Contact>(
        "SELECT * FROM contacts WHERE tenant_id = $1 ORDER BY name ASC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

pub async fn create_contact(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateContactRequest>,
) -> Result<Json<Contact>, AppError> {
    validate_contact(
        Some(&req.name),
        Some(&req.phone),
        req.email.as_deref(),
        req.company.as_deref(),
        req.notes.as_deref(),
        req.tags.as_ref(),
    )?;

    features::enforce_feature_limit(&state.pool, claims.aid, "max_contacts", "Contacts").await?;

    // Check for duplicate email within tenant
    if let Some(ref email) = req.email {
        if !email.trim().is_empty() {
            let existing: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM contacts WHERE email = $1 AND tenant_id = $2)",
            )
            .bind(email)
            .bind(claims.aid)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(false);

            if existing {
                return Err(AppError::BadRequest(format!(
                    "A contact with email '{}' already exists",
                    email
                )));
            }
        }
    }

    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO contacts (id, name, phone, email, company, notes, tags, tenant_id, created_at, updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.phone)
    .bind(&req.email)
    .bind(&req.company)
    .bind(&req.notes)
    .bind(&req.tags)
    .bind(claims.aid)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, Contact>("SELECT * FROM contacts WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;

    // Activity feed (best-effort): a captured contact shows on the customer's dashboard.
    crate::activity::contact_created(
        &state.pool,
        claims.aid,
        Some(claims.sub),
        id,
        &item.name,
        Some(item.phone.as_str()),
    )
    .await;

    // Best-effort pushes (never block on failure)
    let state_clone = state.clone();
    let item_clone = item.clone();
    tokio::spawn(async move {
        super::workflowswift_push::push_contact_to_workflowswift(&state_clone, &item_clone).await;
        // Inbound → CoreSwift (one CoreSwift code path: coreswift_external). Quiet no-op
        // when the tenant has not connected CoreSwift.
        let tags = item_clone.tags.clone().unwrap_or_default();
        super::coreswift_external::push_lead_to_coreswift(
            &state_clone,
            &item_clone.tenant_id,
            &item_clone.name,
            item_clone.company.as_deref(),
            item_clone.email.as_deref(),
            Some(item_clone.phone.as_str()),
            &tags,
            None,
            Some("contact_creation"),
            item_clone.notes.as_deref(),
        )
        .await;
    });

    Ok(Json(item))
}

pub async fn get_contact(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Contact>, AppError> {
    let item =
        sqlx::query_as::<_, Contact>("SELECT * FROM contacts WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Contact not found".into()))?;
    Ok(Json(item))
}

pub async fn update_contact(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateContactRequest>,
) -> Result<Json<Contact>, AppError> {
    validate_contact(
        req.name.as_deref(),
        req.phone.as_deref(),
        req.email.as_deref(),
        req.company.as_deref(),
        req.notes.as_deref(),
        req.tags.as_ref(),
    )?;

    let existing =
        sqlx::query_as::<_, Contact>("SELECT * FROM contacts WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Contact not found".into()))?;

    let now = chrono::Utc::now().naive_utc();
    sqlx::query(
        "UPDATE contacts SET name=$1, phone=$2, email=$3, company=$4, notes=$5, tags=$6, updated_at=$7 WHERE id=$8",
    )
    .bind(req.name.unwrap_or(existing.name))
    .bind(req.phone.unwrap_or(existing.phone))
    .bind(req.email.or(existing.email))
    .bind(req.company.or(existing.company))
    .bind(req.notes.or(existing.notes))
    .bind(req.tags.or(existing.tags))
    .bind(now)
    .bind(id)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, Contact>("SELECT * FROM contacts WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;

    // Best-effort push to CoreSwift (tag changes) — one CoreSwift code path.
    let state_clone = state.clone();
    let item_clone = item.clone();
    tokio::spawn(async move {
        let tags = item_clone.tags.clone().unwrap_or_default();
        super::coreswift_external::push_lead_to_coreswift(
            &state_clone,
            &item_clone.tenant_id,
            &item_clone.name,
            item_clone.company.as_deref(),
            item_clone.email.as_deref(),
            Some(item_clone.phone.as_str()),
            &tags,
            None,
            Some("tag_update"),
            item_clone.notes.as_deref(),
        )
        .await;
    });

    Ok(Json(item))
}

pub async fn delete_contact(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let result = sqlx::query("DELETE FROM contacts WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(claims.aid)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Contact not found".into()));
    }
    Ok(Json(serde_json::json!({"message": "Contact deleted"})))
}

pub async fn search_contacts(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<Contact>>, AppError> {
    let query = params.get("q").map(|s| format!("%{}%", s));
    let phone = params.get("phone");

    let items = if let Some(p) = phone {
        sqlx::query_as::<_, Contact>(
            "SELECT * FROM contacts WHERE tenant_id = $1 AND phone LIKE $2 ORDER BY name ASC",
        )
        .bind(claims.aid)
        .bind(p)
        .fetch_all(&state.pool)
        .await?
    } else if let Some(q) = query {
        sqlx::query_as::<_, Contact>(
            "SELECT * FROM contacts WHERE tenant_id = $1 AND (name ILIKE $2 OR phone ILIKE $2 OR email ILIKE $2) ORDER BY name ASC",
        )
        .bind(claims.aid)
        .bind(&q)
        .fetch_all(&state.pool)
        .await?
    } else {
        sqlx::query_as::<_, Contact>(
            "SELECT * FROM contacts WHERE tenant_id = $1 ORDER BY name ASC",
        )
        .bind(claims.aid)
        .fetch_all(&state.pool)
        .await?
    };

    Ok(Json(items))
}
