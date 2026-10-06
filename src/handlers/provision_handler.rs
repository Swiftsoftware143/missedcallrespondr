//! FunnelSwift tag → free account: the receiver (design §3.1, kanban t_1d08bd9a).
//!
//! `POST /api/v1/internal/provision-free-account` is the frozen contract every target app answers,
//! so FunnelSwift's one generic client (`FunnelSwift/src/app_provision.rs`, caller card t_847f9d63)
//! can call them all:
//!
//!     POST /api/v1/internal/provision-free-account
//!     x-internal-key: <shared INTERNAL_SYNC_KEY>          (fail closed on empty/unset)
//!     { source, source_tenant_id, tag:{name,plan_slug}, contact:{...}, idempotency_key }
//!   → 201 provisioned | 200 already_exists | 403 refused | 422 invalid
//!
//! ## Why this route exists
//!
//! MissedCall Respondr shipped a `/api/v1/internal/tag-provision` receiver until 2026-10-01
//! (t_c2353c90), and its deletion note said: *do not re-add a receiver here without a caller*.
//! The caller now exists (FunnelSwift's tag orchestrator, `t_847f9d63`) and so does the contract
//! above, so this is that receiver re-added under the frozen name. The OLD handler was deleted for a
//! real reason worth restating: it filed every lead into ONE hardcoded tenant
//! (`883a2a82-…`, kanban t_c9669881), i.e. a write into a workspace nobody could log into — the
//! `FunnelSwift Leads` holder shape this card retires. **That objection does not apply here**:
//! nothing in this request names a tenant to write into. The caller sends a contact address, the
//! entry plan is resolved in-app, and the account is minted for that address by this app's own
//! signup core (`crate::auth::signup::create_account`) — the same single writer the public
//! `POST /api/v1/auth/register` uses. There is no "which tenant?" question left to answer
//! arbitrarily, and the residue of the old answer is retired by `migrations/000030`.
//!
//! ## Reachability
//!
//! Mounted on the anonymous router (a sibling app presents a key, not a session) and named in
//! `crate::auth::route_policy::INTERNAL_ROUTES`, so the one credential boundary
//! (`crate::auth::boundary::require_credential`) demands `x-internal-key` BEFORE the handler runs:
//! without the key the caller gets the boundary's own 401 and never reaches this code. The handler
//! checks the key again (fail-closed on an empty configured key), so the route is refused twice
//! over rather than once.

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{extract::Extension, extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::Claims;
use crate::error::AppError;
use crate::security::email_addr;
use crate::state::AppState;

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Frozen request/response vocabulary (design §3.1)
// ─────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ProvisionFreeAccountRequest {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub source_tenant_id: Option<String>,
    #[serde(default)]
    pub tag: Option<ProvisionFreeAccountTag>,
    pub contact: ProvisionFreeAccountContact,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProvisionFreeAccountTag {
    #[serde(default)]
    pub name: Option<String>,
    /// The plan the SENDER believes this app should seat. Deliberately UNUSED: design §3.1 rule 1
    /// resolves the entry plan IN-APP (`admin_settings.provision_entry_plan_slug`, see
    /// [`resolve_entry_plan`]) and never from a sibling's plan name, so a caller cannot name a plan
    /// in this app. Kept in the struct because the contract is frozen and FunnelSwift sends it.
    #[serde(default)]
    #[allow(dead_code)]
    pub plan_slug: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProvisionFreeAccountContact {
    pub email: String,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub company: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
}

fn refused(reason: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "status": "refused", "reason": reason })),
    )
        .into_response()
}

fn invalid(reason: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "status": "invalid", "reason": reason })),
    )
        .into_response()
}

fn already_exists(account_id: Option<Uuid>) -> Response {
    (
        StatusCode::OK,
        Json(json!({
            "status": "already_exists",
            "account_id": account_id.map(|a| a.to_string()).unwrap_or_default(),
        })),
    )
        .into_response()
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// admin_settings helpers + the two values this feature owns
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// Read one `admin_settings` value as a bool; a missing or non-bool value yields `default`.
pub async fn admin_setting_bool(
    db: &sqlx::PgPool,
    key: &str,
    default: bool,
) -> Result<bool, AppError> {
    let v: Option<Value> = sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await?;
    Ok(v.and_then(|j| j.as_bool()).unwrap_or(default))
}

/// Read one `admin_settings` value as a string; a missing or non-string value yields `default`.
pub async fn admin_setting_str(
    db: &sqlx::PgPool,
    key: &str,
    default: &str,
) -> Result<String, AppError> {
    let v: Option<Value> = sqlx::query_scalar("SELECT value FROM admin_settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await?;
    Ok(v.and_then(|j| j.as_str().map(str::to_string))
        .unwrap_or_else(|| default.to_string()))
}

async fn upsert_admin_setting(
    db: &sqlx::PgPool,
    key: &str,
    value: Value,
    description: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO admin_settings (key, value, description, updated_at) VALUES ($1, $2::jsonb, $3, now()) \
         ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, \
           description = COALESCE(admin_settings.description, EXCLUDED.description), updated_at = now()",
    )
    .bind(key)
    .bind(value)
    .bind(description)
    .execute(db)
    .await?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Small pure helpers (unit-tested below)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// A server-generated temporary password from the same shape the app's paid-checkout credential
/// delivery uses (`checkout_handler::generate_temp_password` — 12 chars), minus the glyphs that are
/// indistinguishable in a mail client's proportional font (`I`, `l`, `1`, `O`, `0`). The customer
/// reads this out of an email and types it into a login form, so the alphabet matters.
fn generate_password() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789!@#";
    let mut rng = rand::thread_rng();
    (0..12)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// A fabricated address must never mint an account (design §3.1 rule 5). The old CoreSwift
/// `fs-provision-…@placeholder` fallback is exactly what this refuses.
fn looks_like_placeholder(email: &str) -> bool {
    email.contains("placeholder")
        || email.starts_with("fs-provision")
        || email.ends_with(".invalid")
}

/// The entry plan this app will seat a tag-provisioned account on, resolved IN-APP (design §3.1
/// rule 1): the `admin_settings` slug, DEFAULT `free`. A plan qualifies only when it is active AND
/// free in BOTH price columns — `plans` carries a legacy `price` and the `price_monthly` the panel
/// edits, and a plan is only honestly "free" when neither can charge (they are kept in step by
/// `migrations/000029`).
async fn resolve_entry_plan(
    db: &sqlx::PgPool,
    slug: &str,
) -> Result<Option<(Uuid, String)>, AppError> {
    sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, slug FROM plans WHERE slug = $1 AND is_active = true \
         AND COALESCE(price_monthly, 0) = 0 AND COALESCE(price, 0) = 0 LIMIT 1",
    )
    .bind(slug)
    .fetch_optional(db)
    .await
    .map_err(AppError::from)
}

/// Every plan this app can honestly call free, for the console's entry-plan picker.
async fn free_plans(db: &sqlx::PgPool) -> Result<Vec<(String, String)>, AppError> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT slug, name FROM plans WHERE is_active = true \
         AND COALESCE(price_monthly, 0) = 0 AND COALESCE(price, 0) = 0 \
         ORDER BY sort_order ASC, name ASC",
    )
    .fetch_all(db)
    .await
    .map_err(AppError::from)
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// The receiver
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// The receiver FunnelSwift's tag→free-account orchestrator calls (design §3.1).
///
/// Order of checks matters: the credential fails closed first, then the master toggle refuses
/// before anything is read or written, then the address and the entry plan are validated, then
/// idempotency, then the mint.
pub async fn provision_free_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ProvisionFreeAccountRequest>,
) -> Result<Response, AppError> {
    // 1. Credential — fail closed when the key is unset on EITHER side. The boundary
    //    (`auth::boundary::require_credential`) already demands this key for this path, but it also
    //    admits a recognised session credential, so a user JWT can reach this handler: it must still
    //    be refused here, not only at the edge. Same expression as the app's other two internal
    //    receivers, so the empty-key class (t_eb7736b8) cannot come back through this door.
    let expected = state.config.internal_sync_key.as_str();
    let presented = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if expected.is_empty() || presented.is_empty() || presented != expected {
        return Err(AppError::Unauthorized("Invalid internal key".into()));
    }

    // 2. Master toggle — ships OFF; David enables it per app from the console (design §3.1 rule 2).
    if !admin_setting_bool(&state.pool, "provision_from_tags_enabled", false).await? {
        return Ok(refused("provisioning_disabled"));
    }

    // 3. Address — normalise (a non-address is refused 422) and refuse a placeholder outright.
    let email = email_addr::normalize(&req.contact.email).map_err(AppError::Unprocessable)?;
    if looks_like_placeholder(&email) {
        return Ok(invalid("placeholder_email"));
    }

    // 4. Entry plan — resolved IN-APP by slug, never by a sibling's plan name (design §3.1 rule 1).
    let plan_slug = admin_setting_str(&state.pool, "provision_entry_plan_slug", "free").await?;
    let Some((plan_id, resolved_slug)) = resolve_entry_plan(&state.pool, &plan_slug).await? else {
        return Ok(invalid("no_free_plan"));
    };

    // 5. Idempotent by LOWER(email): an existing account mints nothing (design §3.1 rule 3).
    let existing: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, tenant_id FROM users WHERE lower(email) = $1 \
         ORDER BY created_at ASC, id ASC LIMIT 1",
    )
    .bind(&email)
    .fetch_optional(&state.pool)
    .await?;
    if let Some((_, tenant_id)) = existing {
        return Ok(already_exists(Some(tenant_id)));
    }

    // 6. Mint through the ONE shared signup core (design §3.1 rule 4): tenants + an `account_owner`
    //    users row + tenant_plans(<entry plan>, 50 starter credits) — exactly the shape the public
    //    signup mints. A server-generated password is mailed through the app's existing
    //    `welcome_credentials` template, so the business can log in and upgrade in place.
    let first = req.contact.first_name.clone().unwrap_or_default();
    let last = req.contact.last_name.clone().unwrap_or_default();
    let name = {
        let full = format!("{} {}", first.trim(), last.trim());
        let full = full.trim().to_string();
        if full.is_empty() {
            req.contact
                .company
                .clone()
                .filter(|c| !c.trim().is_empty())
                .unwrap_or_else(|| email.clone())
        } else {
            full
        }
    };
    // The workspace label. Deliberately NOT the tag's name or the source app: a tenant name is what
    // the business sees in its own console, and this app must not stamp one application's marketing
    // vocabulary onto a customer's workspace (the retired `FunnelSwift Leads` holder did exactly
    // that). Company first, then a neutral workspace label derived from the contact.
    let account_name = req
        .contact
        .company
        .clone()
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| format!("{name}'s Workspace"));

    let raw_password = generate_password();
    let password_hash = crate::auth::models::hash_password(&raw_password)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let account_slug = crate::auth::signup::unique_account_slug(&state.pool, &account_name).await?;

    let ids = match crate::auth::signup::create_account(
        &state,
        crate::auth::signup::NewAccount {
            email: &email,
            name: &name,
            password_hash: &password_hash,
            password_plain: Some(&raw_password),
            account_name: &account_name,
            account_slug: Some(&account_slug),
            plan_slug: &resolved_slug,
            role: "account_owner",
        },
    )
    .await
    {
        Ok(ids) => ids,
        // The idempotency check runs before the mint, but two concurrent calls can still race (or a
        // second delivery can arrive while the first is mid-flight). Answer `already_exists`
        // truthfully rather than 500-ing the caller.
        Err(AppError::Conflict(_)) => {
            let account_id: Option<Uuid> = sqlx::query_scalar(
                "SELECT tenant_id FROM users WHERE lower(email) = $1 \
                 ORDER BY created_at ASC, id ASC LIMIT 1",
            )
            .bind(&email)
            .fetch_optional(&state.pool)
            .await?;
            return Ok(already_exists(account_id));
        }
        Err(other) => return Err(other),
    };

    let source = req
        .source
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "funnelswift".to_string());
    // The whole delivery is logged so an operator can correlate a FunnelSwift attempt with the
    // account it produced: the sender's tenant, the tag, and the sender's own idempotency key. The
    // phone is reported as a presence flag only — it is PII and this app stores no contact row for a
    // provisioned account, it only proves the caller sent one.
    tracing::info!(
        account = %ids.account_id,
        user = %ids.user_id,
        email = %email,
        workspace = %ids.account_name,
        slug = %ids.account_slug,
        source = %source,
        source_tenant_id = ?req.source_tenant_id,
        tag = ?req.tag.as_ref().and_then(|t| t.name.as_deref()),
        idempotency_key = ?req.idempotency_key,
        phone_provided = req.contact.phone.is_some(),
        plan = %resolved_slug,
        plan_id = %plan_id,
        "provision_free_account: minted a free account from a tag"
    );

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "status": "provisioned",
            "account_id": ids.account_id.to_string(),
            "plan_slug": resolved_slug,
            "login_email": email,
        })),
    )
        .into_response())
}

// ─────────────────────────────────────────────────────────────────────────────────────────────
// Admin console: the master toggle + the in-app entry-plan picker (design §3.3)
// ─────────────────────────────────────────────────────────────────────────────────────────────

/// GET the two provisioning settings plus this app's own free plans, for the picker.
///
/// Platform-admin only — the role gate is `auth::middleware::admin_surface_denied`, which refuses
/// every non-platform-admin on the whole `/api/v1/admin/*` prefix at the choke point (403 with the
/// caller's own role in the body). So this handler carries no role check of its own, on purpose:
/// there is exactly ONE place that decides the admin surface, and adding a second would be the
/// drift this fleet already fixed once (kanban t_92ec2f21).
pub async fn get_provisioning_settings(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> Result<Json<Value>, AppError> {
    let enabled = admin_setting_bool(&state.pool, "provision_from_tags_enabled", false).await?;
    let plan_slug = admin_setting_str(&state.pool, "provision_entry_plan_slug", "free").await?;
    let plans = free_plans(&state.pool).await?;
    Ok(Json(json!({
        "provision_from_tags_enabled": enabled,
        "provision_entry_plan_slug": plan_slug,
        "free_plans": plans
            .into_iter()
            .map(|(slug, name)| json!({ "slug": slug, "name": name }))
            .collect::<Vec<_>>(),
    })))
}

/// Update the two settings. The plan slug must resolve to a real FREE plan in THIS app — the panel
/// cannot point provisioning at a paid plan.
pub async fn update_provisioning_settings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    if let Some(v) = body.get("provision_from_tags_enabled") {
        let b = v.as_bool().ok_or_else(|| {
            AppError::BadRequest("provision_from_tags_enabled must be a boolean".into())
        })?;
        upsert_admin_setting(
            &state.pool,
            "provision_from_tags_enabled",
            json!(b),
            "Tag → free account: master switch (design §3.1 rule 2)",
        )
        .await?;
    }
    if let Some(v) = body.get("provision_entry_plan_slug") {
        let s = v.as_str().ok_or_else(|| {
            AppError::BadRequest("provision_entry_plan_slug must be a string".into())
        })?;
        if resolve_entry_plan(&state.pool, s).await?.is_none() {
            return Err(AppError::BadRequest(format!(
                "'{s}' is not an active free plan in MissedCall Respondr"
            )));
        }
        upsert_admin_setting(
            &state.pool,
            "provision_entry_plan_slug",
            json!(s),
            "Tag → free account: entry plan slug (design §3.1 rule 1)",
        )
        .await?;
    }
    // Answer with the same shape the GET does (the console re-renders from this response).
    get_provisioning_settings(State(state), Extension(claims)).await
}

#[cfg(test)]
mod tests {
    use super::{generate_password, looks_like_placeholder};

    #[test]
    fn generated_password_is_twelve_chars_from_the_credential_charset() {
        for _ in 0..50 {
            let p = generate_password();
            assert_eq!(p.len(), 12, "password length: {p}");
            assert!(
                p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "!@#".contains(c)),
                "unexpected char in {p}"
            );
            assert!(
                !p.contains(['I', 'l', '1', 'O', '0']),
                "look-alike glyph in {p}"
            );
        }
        let a = generate_password();
        let b = generate_password();
        assert_ne!(a, b, "two consecutive passwords were identical");
    }

    #[test]
    fn placeholder_addresses_are_refused_and_real_ones_are_not() {
        assert!(looks_like_placeholder("fs-provision-x@example.com"));
        assert!(looks_like_placeholder("someone@probe.invalid"));
        assert!(looks_like_placeholder("placeholder@example.com"));
        assert!(!looks_like_placeholder("owner@acme.example.com"));
        assert!(!looks_like_placeholder("david@swiftsoftware.dev"));
    }
}
