//! Tag provision webhook — receives FunnelSwift system tag assignments.

use axum::response::IntoResponse;
use axum::{extract::State, http::HeaderMap, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

/// Owner of contacts auto-provisioned from FunnelSwift tag assignments, by SLUG (never a uuid).
/// Must match the tenant migrations/000018_funnelswift_tenant.sql seeds, and is overridable with
/// `TAG_PROVISION_TENANT_SLUG`.
pub const DEFAULT_TENANT_SLUG: &str = "funnelswift";
/// Name given to the owner tenant if this handler is the one that creates it (first provision on a
/// database where the slug is absent).
pub const DEFAULT_TENANT_NAME: &str = "FunnelSwift Leads";

#[derive(Debug, Deserialize)]
pub struct TagProvisionRequest {
    pub contact: TagProvisionContact,
    pub tag: TagProvisionTag,
    #[allow(dead_code)]
    pub source: String,
    #[allow(dead_code)]
    pub timestamp: String,
}

#[derive(Debug, Deserialize)]
pub struct TagProvisionContact {
    #[allow(dead_code)]
    pub id: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company: Option<String>,
    #[allow(dead_code)]
    pub custom_fields: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct TagProvisionTag {
    pub name: String,
    #[allow(dead_code)]
    pub campaign_id: Option<String>,
    #[allow(dead_code)]
    pub metadata: Option<Value>,
}

pub async fn handle_tag_provision(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TagProvisionRequest>,
) -> Result<impl IntoResponse, AppError> {
    let key = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = state.config.internal_sync_key.as_str();
    if expected.is_empty() || key != expected {
        // Lengths only. This line used to print the PRESENTED key, so any caller that sent its own
        // internal key to this endpoint had that key written into this app's logs (aggregated, and
        // copied into /opt/swift/audits at mode 644) — the log half of class t_eb7736b8.
        tracing::warn!(
            "tag_provision: invalid internal key (presented_len={}, configured_len={})",
            key.len(),
            expected.len()
        );
        return Err(AppError::Unauthorized("Invalid internal key".into()));
    }

    let email = req
        .contact
        .email
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let first_name = req
        .contact
        .first_name
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let last_name = req
        .contact
        .last_name
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let company_name = req
        .contact
        .company
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let phone_val = req
        .contact
        .phone
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let phone = if phone_val.is_empty() {
        "tag-provision".to_string()
    } else {
        phone_val
    };

    tracing::info!("tag_provision: tag={} email={}", req.tag.name, email);

    // Check existing
    if !email.is_empty() {
        let existing: Option<(Uuid,)> =
            sqlx::query_as(r#"SELECT id FROM contacts WHERE email = $1 LIMIT 1"#)
                .bind(&email)
                .fetch_optional(&state.pool)
                .await?;

        if let Some((contact_id,)) = existing {
            return Ok((
                axum::http::StatusCode::OK,
                Json(json!({
                    "status": "already_exists",
                    "contact_id": contact_id.to_string(),
                })),
            ));
        }
    }

    let contact_id = Uuid::new_v4();
    let contact_name = if !first_name.is_empty() && !last_name.is_empty() {
        format!("{} {}", first_name, last_name)
    } else if !first_name.is_empty() {
        first_name.clone()
    } else if !company_name.is_empty() {
        company_name.clone()
    } else {
        format!("FS-Lead-{}", &contact_id.to_string()[..8])
    };

    let notes = format!("Auto-provisioned via FunnelSwift tag: {}", req.tag.name);

    // Who owns a contact that arrives from FunnelSwift: resolved by NAME at runtime, because the
    // uuid this used to bind existed in no database and made every provision 500 on
    // contacts_tenant_id_fkey (t_c9669881).
    let tenant_slug = provision_tenant_slug(&state.config);
    let tenant_id = resolve_provision_tenant(&state.pool, tenant_slug).await?;

    sqlx::query(
        r#"INSERT INTO contacts (id, name, email, phone, company, notes, tenant_id, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())"#
    )
    .bind(contact_id)
    .bind(&contact_name)
    .bind(if email.is_empty() { None } else { Some(&email) })
    .bind(&phone)
    .bind(if company_name.is_empty() { None } else { Some(&company_name) })
    .bind(&notes)
    .bind(tenant_id)
    .execute(&state.pool)
    .await?;

    tracing::info!(
        "tag_provision: created contact {} in tenant slug={} (id={})",
        contact_id,
        tenant_slug,
        tenant_id
    );

    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({
            "status": "provisioned",
            "contact_id": contact_id.to_string(),
            "tenant_id": tenant_id.to_string(),
            "tenant_slug": tenant_slug,
        })),
    ))
}

/// Slug of the tenant that owns FunnelSwift-provisioned contacts.
///
/// `TAG_PROVISION_TENANT_SLUG` (default `funnelswift`, same slug
/// migrations/000018_funnelswift_tenant.sql seeds). An empty/whitespace value falls back to the
/// default rather than querying for slug `''`, which no tenant can have.
pub fn provision_tenant_slug(config: &crate::config::AppConfig) -> &str {
    let slug = config.tag_provision_tenant_slug.trim();
    if slug.is_empty() {
        DEFAULT_TENANT_SLUG
    } else {
        slug
    }
}

/// Resolve the owner tenant for auto-provisioned contacts, creating it on first use.
///
/// Get-or-create by SLUG — never a uuid literal, so no environment can end up holding an owner id
/// that does not exist (the defect this replaces: the hardcoded
/// `883a2a82-c7e4-4abb-b6c2-da47c119caf1` was in no database, so every provision died on
/// contacts_tenant_id_fkey with a 500). `ON CONFLICT (slug) DO NOTHING` + re-select mirrors the
/// conflict-tolerant get-or-create the sibling tag-sync uses (CoreSwift t_2dfaffa4), so two
/// concurrent first-provisions cannot both fail.
pub async fn resolve_provision_tenant(pool: &sqlx::PgPool, slug: &str) -> Result<Uuid, AppError> {
    if let Some((id,)) =
        sqlx::query_as::<_, (Uuid,)>("SELECT id FROM tenants WHERE slug = $1 LIMIT 1")
            .bind(slug)
            .fetch_optional(pool)
            .await?
    {
        return Ok(id);
    }

    // First provision on this database (or the owner tenant was removed): create it.
    sqlx::query(
        r#"INSERT INTO tenants (id, name, slug, created_at, updated_at, is_active)
           VALUES ($1, $2, $3, NOW(), NOW(), true)
           ON CONFLICT (slug) DO NOTHING"#,
    )
    .bind(Uuid::new_v4())
    .bind(DEFAULT_TENANT_NAME)
    .bind(slug)
    .execute(pool)
    .await?;

    // Whoever won the race is the owner.
    let (id,) = sqlx::query_as::<_, (Uuid,)>("SELECT id FROM tenants WHERE slug = $1 LIMIT 1")
        .bind(slug)
        .fetch_one(pool)
        .await?;
    tracing::info!(
        "tag_provision: created owner tenant slug={} (id={})",
        slug,
        id
    );
    Ok(id)
}
