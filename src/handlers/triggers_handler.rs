//! Campaign trigger handlers for MissedCall Respondr
//!
//! PerkZilla-style trigger system: email + redirect triggers on campaign events.
//! Events: on_win, on_enter, on_quiz_result, on_loss, on_raffle_entry
//!
//! Endpoints:
//!   GET/POST   /api/v1/triggers/email
//!   GET/PUT/DELETE /api/v1/triggers/email/:id
//!   GET/POST   /api/v1/triggers/redirect
//!   GET/PUT/DELETE /api/v1/triggers/redirect/:id
//!   PUT        /api/v1/portfolio-companies/:id/smtp

use axum::{
    extract::{Path, State, Extension},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use chrono::{DateTime, Utc};
use crate::config::Claims;
use crate::error::AppError;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// Email Trigger Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct EmailTrigger {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub portfolio_company_id: Option<Uuid>,
    pub name: String,
    pub trigger_event: String,
    pub subject_template: String,
    pub body_template: String,
    pub from_name: Option<String>,
    pub from_email: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateEmailTriggerInput {
    pub portfolio_company_id: Option<String>,
    pub name: String,
    pub trigger_event: Option<String>,
    pub subject_template: String,
    pub body_template: String,
    pub from_name: Option<String>,
    pub from_email: Option<String>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEmailTriggerInput {
    pub name: Option<String>,
    pub trigger_event: Option<String>,
    pub subject_template: Option<String>,
    pub body_template: Option<String>,
    pub from_name: Option<String>,
    pub from_email: Option<String>,
    pub is_active: Option<bool>,
}

// ---------------------------------------------------------------------------
// Redirect Trigger Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RedirectTrigger {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub portfolio_company_id: Option<Uuid>,
    pub name: String,
    pub trigger_event: String,
    pub redirect_url: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRedirectTriggerInput {
    pub portfolio_company_id: Option<String>,
    pub name: String,
    pub trigger_event: Option<String>,
    pub redirect_url: String,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRedirectTriggerInput {
    pub name: Option<String>,
    pub trigger_event: Option<String>,
    pub redirect_url: Option<String>,
    pub is_active: Option<bool>,
}

// ---------------------------------------------------------------------------
// SMTP Config Types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct SmtpConfig {
    pub smtp_provider: Option<String>,
    pub smtp_api_key: Option<String>,
    pub smtp_from_email: Option<String>,
    pub smtp_from_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Email Trigger Handlers
// ---------------------------------------------------------------------------

/// GET /api/v1/triggers/email
pub async fn list_email_triggers(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let triggers = sqlx::query_as::<_, EmailTrigger>(
        "SELECT * FROM campaign_email_triggers WHERE tenant_id = $1 ORDER BY name"
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(json!({ "triggers": triggers })))
}

/// POST /api/v1/triggers/email
pub async fn create_email_trigger(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(body): Json<CreateEmailTriggerInput>,
) -> Result<Json<Value>, AppError> {
    if body.name.trim().is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    if body.subject_template.trim().is_empty() {
        return Err(AppError::BadRequest("subject_template is required".into()));
    }
    if body.body_template.trim().is_empty() {
        return Err(AppError::BadRequest("body_template is required".into()));
    }

    let portfolio_id = if let Some(ref pid) = body.portfolio_company_id {
        Some(Uuid::parse_str(pid).map_err(|_| AppError::BadRequest("Invalid portfolio_company_id".into()))?)
    } else {
        None
    };

    let trigger_event = body.trigger_event.unwrap_or_else(|| "on_win".to_string());
    let valid_events = ["on_win", "on_enter", "on_quiz_result", "on_loss", "on_raffle_entry"];
    if !valid_events.contains(&trigger_event.as_str()) {
        return Err(AppError::BadRequest(format!("Invalid trigger_event. Must be one of: {}", valid_events.join(", "))));
    }

    let id = Uuid::new_v4();
    let now = Utc::now();

    sqlx::query(
        r#"INSERT INTO campaign_email_triggers
           (id, tenant_id, portfolio_company_id, name, trigger_event, subject_template, body_template, from_name, from_email, is_active, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)"#
    )
    .bind(id)
    .bind(claims.aid)
    .bind(portfolio_id)
    .bind(body.name.trim())
    .bind(&trigger_event)
    .bind(body.subject_template.trim())
    .bind(body.body_template.trim())
    .bind(body.from_name.as_deref())
    .bind(body.from_email.as_deref())
    .bind(body.is_active.unwrap_or(true))
    .bind(now)
    .execute(&state.pool)
    .await?;

    let trigger = sqlx::query_as::<_, EmailTrigger>(
        "SELECT * FROM campaign_email_triggers WHERE id = $1"
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(json!({ "trigger": trigger })))
}

/// GET /api/v1/triggers/email/:id
pub async fn get_email_trigger(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let trigger_id = Uuid::parse_str(&id)
        .map_err(|_| AppError::BadRequest("Invalid ID".into()))?;

    let trigger = sqlx::query_as::<_, EmailTrigger>(
        "SELECT * FROM campaign_email_triggers WHERE id = $1 AND tenant_id = $2"
    )
    .bind(trigger_id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Email trigger not found".into()))?;

    Ok(Json(json!({ "trigger": trigger })))
}

/// PUT /api/v1/triggers/email/:id
pub async fn update_email_trigger(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateEmailTriggerInput>,
) -> Result<Json<Value>, AppError> {
    let trigger_id = Uuid::parse_str(&id)
        .map_err(|_| AppError::BadRequest("Invalid ID".into()))?;

    // Verify ownership
    let existing = sqlx::query_as::<_, EmailTrigger>(
        "SELECT * FROM campaign_email_triggers WHERE id = $1 AND tenant_id = $2"
    )
    .bind(trigger_id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Email trigger not found".into()))?;

    let name = body.name.unwrap_or(existing.name);
    let trigger_event = body.trigger_event.unwrap_or(existing.trigger_event);
    let subject_template = body.subject_template.unwrap_or(existing.subject_template);
    let body_template = body.body_template.unwrap_or(existing.body_template);
    let from_name = body.from_name.or(existing.from_name);
    let from_email = body.from_email.or(existing.from_email);
    let is_active = body.is_active.unwrap_or(existing.is_active);

    sqlx::query(
        r#"UPDATE campaign_email_triggers
           SET name = $1, trigger_event = $2, subject_template = $3, body_template = $4,
               from_name = $5, from_email = $6, is_active = $7, updated_at = NOW()
           WHERE id = $8"#
    )
    .bind(&name)
    .bind(&trigger_event)
    .bind(&subject_template)
    .bind(&body_template)
    .bind(&from_name)
    .bind(&from_email)
    .bind(is_active)
    .bind(trigger_id)
    .execute(&state.pool)
    .await?;

    let trigger = sqlx::query_as::<_, EmailTrigger>(
        "SELECT * FROM campaign_email_triggers WHERE id = $1"
    )
    .bind(trigger_id)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(json!({ "trigger": trigger })))
}

/// DELETE /api/v1/triggers/email/:id
pub async fn delete_email_trigger(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let trigger_id = Uuid::parse_str(&id)
        .map_err(|_| AppError::BadRequest("Invalid ID".into()))?;

    let result = sqlx::query(
        "DELETE FROM campaign_email_triggers WHERE id = $1 AND tenant_id = $2"
    )
    .bind(trigger_id)
    .bind(claims.aid)
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Email trigger not found".into()));
    }

    Ok(Json(json!({ "status": "deleted" })))
}

// ---------------------------------------------------------------------------
// Redirect Trigger Handlers
// ---------------------------------------------------------------------------

/// GET /api/v1/triggers/redirect
pub async fn list_redirect_triggers(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let triggers = sqlx::query_as::<_, RedirectTrigger>(
        "SELECT * FROM campaign_redirect_triggers WHERE tenant_id = $1 ORDER BY name"
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(json!({ "triggers": triggers })))
}

/// POST /api/v1/triggers/redirect
pub async fn create_redirect_trigger(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(body): Json<CreateRedirectTriggerInput>,
) -> Result<Json<Value>, AppError> {
    if body.name.trim().is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    if body.redirect_url.trim().is_empty() {
        return Err(AppError::BadRequest("redirect_url is required".into()));
    }

    let portfolio_id = if let Some(ref pid) = body.portfolio_company_id {
        Some(Uuid::parse_str(pid).map_err(|_| AppError::BadRequest("Invalid portfolio_company_id".into()))?)
    } else {
        None
    };

    let trigger_event = body.trigger_event.unwrap_or_else(|| "on_win".to_string());
    let valid_events = ["on_win", "on_enter", "on_quiz_result", "on_loss", "on_raffle_entry"];
    if !valid_events.contains(&trigger_event.as_str()) {
        return Err(AppError::BadRequest("Invalid trigger_event".to_string()));
    }

    let id = Uuid::new_v4();
    let now = Utc::now();

    sqlx::query(
        r#"INSERT INTO campaign_redirect_triggers
           (id, tenant_id, portfolio_company_id, name, trigger_event, redirect_url, is_active, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)"#
    )
    .bind(id)
    .bind(claims.aid)
    .bind(portfolio_id)
    .bind(body.name.trim())
    .bind(&trigger_event)
    .bind(body.redirect_url.trim())
    .bind(body.is_active.unwrap_or(true))
    .bind(now)
    .execute(&state.pool)
    .await?;

    let trigger = sqlx::query_as::<_, RedirectTrigger>(
        "SELECT * FROM campaign_redirect_triggers WHERE id = $1"
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(json!({ "trigger": trigger })))
}

/// GET /api/v1/triggers/redirect/:id
pub async fn get_redirect_trigger(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let trigger_id = Uuid::parse_str(&id)
        .map_err(|_| AppError::BadRequest("Invalid ID".into()))?;

    let trigger = sqlx::query_as::<_, RedirectTrigger>(
        "SELECT * FROM campaign_redirect_triggers WHERE id = $1 AND tenant_id = $2"
    )
    .bind(trigger_id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Redirect trigger not found".into()))?;

    Ok(Json(json!({ "trigger": trigger })))
}

/// PUT /api/v1/triggers/redirect/:id
pub async fn update_redirect_trigger(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateRedirectTriggerInput>,
) -> Result<Json<Value>, AppError> {
    let trigger_id = Uuid::parse_str(&id)
        .map_err(|_| AppError::BadRequest("Invalid ID".into()))?;

    let existing = sqlx::query_as::<_, RedirectTrigger>(
        "SELECT * FROM campaign_redirect_triggers WHERE id = $1 AND tenant_id = $2"
    )
    .bind(trigger_id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Redirect trigger not found".into()))?;

    let name = body.name.unwrap_or(existing.name);
    let trigger_event = body.trigger_event.unwrap_or(existing.trigger_event);
    let redirect_url = body.redirect_url.unwrap_or(existing.redirect_url);
    let is_active = body.is_active.unwrap_or(existing.is_active);

    sqlx::query(
        r#"UPDATE campaign_redirect_triggers
           SET name = $1, trigger_event = $2, redirect_url = $3, is_active = $4, updated_at = NOW()
           WHERE id = $5"#
    )
    .bind(&name)
    .bind(&trigger_event)
    .bind(&redirect_url)
    .bind(is_active)
    .bind(trigger_id)
    .execute(&state.pool)
    .await?;

    let trigger = sqlx::query_as::<_, RedirectTrigger>(
        "SELECT * FROM campaign_redirect_triggers WHERE id = $1"
    )
    .bind(trigger_id)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(json!({ "trigger": trigger })))
}

/// DELETE /api/v1/triggers/redirect/:id
pub async fn delete_redirect_trigger(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let trigger_id = Uuid::parse_str(&id)
        .map_err(|_| AppError::BadRequest("Invalid ID".into()))?;

    let result = sqlx::query(
        "DELETE FROM campaign_redirect_triggers WHERE id = $1 AND tenant_id = $2"
    )
    .bind(trigger_id)
    .bind(claims.aid)
    .execute(&state.pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Redirect trigger not found".into()));
    }

    Ok(Json(json!({ "status": "deleted" })))
}

// ---------------------------------------------------------------------------
// SMTP Config Handlers
// ---------------------------------------------------------------------------

/// GET /api/v1/portfolio-companies/:id/smtp
pub async fn get_smtp_config(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let pc_id = Uuid::parse_str(&id)
        .map_err(|_| AppError::BadRequest("Invalid portfolio company ID".into()))?;

    let row = sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>, Option<String>)>(
        "SELECT smtp_provider, smtp_api_key, smtp_from_email, smtp_from_name
         FROM portfolio_companies WHERE id = $1 AND tenant_id = $2"
    )
    .bind(pc_id)
    .bind(claims.aid)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Portfolio company not found".into()))?;

    // Mask the API key for security
    let api_key = row.1.as_ref().map(|k| {
        if k.len() > 8 {
            format!("{}...{}", &k[..4], &k[k.len()-4..])
        } else {
            "****".to_string()
        }
    });

    Ok(Json(json!({
        "smtp_provider": row.0,
        "smtp_api_key": api_key,
        "smtp_from_email": row.2,
        "smtp_from_name": row.3,
        "has_key": row.1.is_some()
    })))
}

/// PUT /api/v1/portfolio-companies/:id/smtp
pub async fn update_smtp_config(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SmtpConfig>,
) -> Result<Json<Value>, AppError> {
    let pc_id = Uuid::parse_str(&id)
        .map_err(|_| AppError::BadRequest("Invalid portfolio company ID".into()))?;

    // Verify ownership
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM portfolio_companies WHERE id = $1 AND tenant_id = $2)"
    )
    .bind(pc_id)
    .bind(claims.aid)
    .fetch_one(&state.pool)
    .await?;

    if !exists {
        return Err(AppError::NotFound("Portfolio company not found".into()));
    }

    sqlx::query(
        r#"UPDATE portfolio_companies
           SET smtp_provider = $1, smtp_api_key = $2, smtp_from_email = $3, smtp_from_name = $4, updated_at = NOW()
           WHERE id = $5"#
    )
    .bind(&body.smtp_provider)
    .bind(&body.smtp_api_key)
    .bind(&body.smtp_from_email)
    .bind(&body.smtp_from_name)
    .bind(pc_id)
    .execute(&state.pool)
    .await?;

    Ok(Json(json!({"message": "SMTP config updated"})))
}
