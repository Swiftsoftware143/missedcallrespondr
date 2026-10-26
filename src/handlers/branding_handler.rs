//! `branding_handler` — the logo + settings half of per-account email branding (kanban t_feab8aff,
//! ported from ADASwift c33fcb8 / FunnelSwift t_c06a32eb).
//!
//! Routes, and they are deliberately asymmetric:
//!
//! * `GET  /api/v1/settings/branding` — authenticated, returns the caller's OWN branding document.
//! * `PUT  /api/v1/settings/branding` — authenticated, writes `brand_name` / `brand_color`, and
//!   PRESERVES the stored `logo_url` when the caller omits it.
//! * `POST /api/v1/settings/branding/logo` — authenticated, the image as EITHER `multipart/form-data`
//!   (the fleet idiom, first non-empty part) OR the RAW body (this app's historical avatar idiom),
//!   the caller's OWN account.
//! * `DELETE /api/v1/settings/branding/logo` — same, removes the logo.
//! * `GET  /api/v1/branding/logo/:tenant_id` — PUBLIC by design (`auth::route_policy::PUBLIC_ROUTES`):
//!   a mail client renders `<img src>` with no credential of any kind, so a logo that needed a token
//!   would simply never appear. The route can only ever return the image one account uploaded, keyed
//!   by an unguessable uuid, with the content type sniffed from the bytes at upload time; an account
//!   with no logo answers 404.
//!
//! The image sniff/cap is NOT re-implemented here — this app's ONE image recogniser
//! ([`crate::auth::handlers::sniff_image`]) is shared, exactly as its avatar upload uses it.
//!
//! `logo_url` is written HERE and nowhere else. `put_branding` preserves the stored value when the
//! account saves name/colour, so a panel echo cannot un-reference a logo.

use axum::{
    body::Body,
    extract::{Extension, Path, Request, State},
    http::{header, HeaderMap},
    response::Response,
    Json,
};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::branding;
use crate::config::Claims;
use crate::error::AppError;
use crate::state::AppState;

/// The URL the console and the mail both point at. Version-stamped because the bytes behind it
/// change while the path stays the same, and both the browser and any cache key on the URL.
fn logo_url_for(tenant_id: Uuid) -> String {
    format!(
        "/api/v1/branding/logo/{tenant_id}?v={}",
        chrono::Utc::now().timestamp()
    )
}

/// Read the stored branding document for this account, defaulting to `{}`.
async fn read_doc(state: &AppState, tenant_id: Uuid) -> Value {
    let mut doc = sqlx::query_scalar::<_, Option<Value>>(
        "SELECT value FROM tenant_settings WHERE tenant_id = $1 AND key = $2",
    )
    .bind(tenant_id)
    .bind(branding::SETTINGS_KEY)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .flatten()
    .unwrap_or_else(|| json!({}));

    // A `logo_url` is a POINTER into `tenant_logos` and the panel renders it as the brand thumbnail
    // while every sender embeds it in the mail header. The pointer and the bytes are written by
    // different paths, so a row removed on its own (restore, hand-run DELETE, residue sweep) would
    // otherwise leave a broken image in both places. `put_branding` preserves `logo_url` from this
    // same read, so stripping it here also stops a dead pointer being re-saved.
    let points_at_logo = doc
        .get("logo_url")
        .and_then(Value::as_str)
        .map(|u| !u.trim().is_empty())
        .unwrap_or(false);
    if points_at_logo && !branding::logo_exists(&state.pool, tenant_id).await {
        if let Some(obj) = doc.as_object_mut() {
            obj.insert("logo_url".to_string(), json!(""));
        }
    }
    doc
}

/// Write the whole branding document for this account (upsert on the composite key).
async fn write_doc(state: &AppState, tenant_id: Uuid, doc: &Value) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO tenant_settings (tenant_id, key, value) VALUES ($1, $2, $3) \
         ON CONFLICT (tenant_id, key) DO UPDATE SET value = EXCLUDED.value",
    )
    .bind(tenant_id)
    .bind(branding::SETTINGS_KEY)
    .bind(doc)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// The stored document as (brand_name, brand_color, logo_url), for a write that must not lose them.
fn fields(doc: &Value) -> (String, String, String) {
    let s = |k: &str| doc.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    (s("brand_name"), s("brand_color"), s("logo_url"))
}

/// `GET /api/v1/settings/branding` — the caller's own branding document.
pub async fn get_branding(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(read_doc(&state, claims.aid).await))
}

/// `PUT /api/v1/settings/branding` — save name/colour, keeping a logo that is still stored.
///
/// An omitted `logo_url` is the normal case (the panel echoes name+colour only), so the stored value
/// is preserved; an explicit `""` clears it (alongside the DELETE route).
pub async fn put_branding(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(incoming): Json<Value>,
) -> Result<Json<Value>, AppError> {
    branding::validate_value(&incoming).map_err(AppError::BadRequest)?;

    let (stored_name, stored_color, stored_logo) = fields(&read_doc(&state, claims.aid).await);
    let name = incoming
        .get("brand_name")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or(stored_name);
    let color = incoming
        .get("brand_color")
        .and_then(Value::as_str)
        .map(|s| s.trim().to_string())
        .unwrap_or(stored_color);
    // preserve the stored logo when the key is absent; honour an explicit value (including "")
    let logo = match incoming.get("logo_url") {
        Some(v) => v.as_str().unwrap_or("").to_string(),
        None => stored_logo,
    };

    let doc = branding::document(&name, &color, &logo);
    write_doc(&state, claims.aid, &doc).await?;
    Ok(Json(doc))
}

/// `POST /api/v1/settings/branding/logo` — store the caller's account logo and return its URL.
///
/// TWO body shapes, ONE handler (fleet-normalised 2026-10-10). The other three apps take the logo as
/// `multipart/form-data`; this app historically took it as the RAW body. A client that used the
/// fleet's form idiom got a 400 "Unsupported logo" here, so the handler now reads BOTH:
/// `multipart/form-data` takes the first non-empty file part, anything else is the body verbatim.
/// The format is decided by the bytes in either arm (never a caller-declared content type), a
/// non-image and an empty body are refused 400, and a body over [`MAX_AVATAR_BYTES`] is refused 400
/// as well. One row per account (upsert), so re-uploading replaces the logo rather than accumulating
/// rows.
pub async fn upload_logo(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Json<Value>, AppError> {
    // The ONE shared image path (crate::image_store): a multipart form OR a raw image body, the
    // 2 MB envelope, the magic-byte sniff, and a part with no filename skipped rather than mistaken
    // for the logo. Same reader the avatar upload uses, so the two cannot drift apart again.
    let (content_type, body) =
        crate::image_store::read_request_image(&state, &headers, request).await?;

    sqlx::query(
        "INSERT INTO tenant_logos (tenant_id, content_type, bytes, updated_at) \
         VALUES ($1, $2, $3, NOW()) \
         ON CONFLICT (tenant_id) DO UPDATE SET content_type = EXCLUDED.content_type, \
         bytes = EXCLUDED.bytes, updated_at = NOW()",
    )
    .bind(claims.aid)
    .bind(&content_type)
    .bind(&body)
    .execute(&state.pool)
    .await?;

    // Write ONLY the logo_url of the branding document, so name/colour survive an upload.
    let (name, color, _) = fields(&read_doc(&state, claims.aid).await);
    let logo_url = logo_url_for(claims.aid);
    let doc = branding::document(&name, &color, &logo_url);
    write_doc(&state, claims.aid, &doc).await?;

    Ok(Json(json!({
        "status": "ok",
        "logo_url": logo_url,
        "content_type": content_type,
        "branding": doc,
    })))
}

/// `DELETE /api/v1/settings/branding/logo` — remove the logo and un-reference it.
pub async fn delete_logo(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let removed = sqlx::query("DELETE FROM tenant_logos WHERE tenant_id = $1")
        .bind(claims.aid)
        .execute(&state.pool)
        .await?
        .rows_affected();

    // Keep name/colour; clear only the reference.
    let (name, color, _) = fields(&read_doc(&state, claims.aid).await);
    write_doc(&state, claims.aid, &branding::document(&name, &color, "")).await?;

    Ok(Json(json!({"status": "ok", "removed": removed})))
}

/// `POST /api/v1/settings/branding/preview` — mail the CALLER a sample of their own branded mail.
///
/// WHY (cron 2026-10-24, parity with FunnelSwift, IncentiveSwift and ADASwift): the Email Branding
/// card lets an operator set a logo, a display name and a colour, but until now they had no way to
/// see the result in a real inbox — the panel's own sample is drawn by the browser, not by the mail
/// pipeline. `send_template_email` already applies that branding to every send, so this arm sends
/// the app's own `branding_preview` sample down the normal path: the operator sees exactly what a
/// caller receives, header and all.
///
/// The recipient is ALWAYS the caller's own address (the request carries no body), so this can
/// never be used as a relay, and an address that cannot receive mail is told so plainly instead of
/// being shown a false "sent".
pub async fn preview_email(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AppError> {
    let to: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
        .bind(claims.sub)
        .fetch_optional(&state.pool)
        .await?;
    let to = to.unwrap_or_default().trim().to_string();
    if to.is_empty() {
        return Err(AppError::BadRequest(
            "This account has no email address to send the preview to".into(),
        ));
    }
    // A reserved/harness address cannot receive mail. Answering with the reason — instead of
    // "sent" — is the whole discipline: the fleet's suppression arm would silently withhold it and
    // the operator would be told a message is on its way that never existed.
    if let Some(domain) = crate::security::probe_addr::harness_domain(&to) {
        return Ok(Json(json!({
            "sent": false,
            "to": to,
            "detail": format!(
                "'{domain}' is a reserved test address, so it cannot receive mail — no preview was sent."
            )
        })));
    }
    let name: String = sqlx::query_scalar("SELECT name FROM tenants WHERE id = $1")
        .bind(claims.aid)
        .fetch_optional(&state.pool)
        .await?
        .filter(|n: &String| !n.trim().is_empty())
        .unwrap_or_else(|| "there".to_string());
    match crate::email::send_branding_preview_email(&state.pool, claims.aid, &to, &name).await {
        Ok(()) => {
            // Say truthfully WHOSE header the reader is about to see. `logo_url` is never empty on
            // the wire now — an account with no branding gets this app's own wordmark — so the
            // answer keys on whether the logo is the account's OWN upload. Before this, an
            // unbranded account was told the preview "carries the MissedCall Respondr header" while
            // the rendered message had no header at all (measured live 2026-10-24, kanban
            // t_6a8af451), and a name-only account was told it carried "your logo".
            let stored = branding::load(&state.pool, claims.aid).await;
            let logo_is_own = stored
                .as_ref()
                .and_then(|b| b.resolve_logo_url(branding::APP_URL))
                .is_some();
            let name_is_own = stored
                .as_ref()
                .map(|b| !b.brand_name.is_empty())
                .unwrap_or(false);
            let detail = if logo_is_own {
                "Preview sent to your own address, with your logo and brand name at the top."
            } else if name_is_own {
                "Preview sent to your own address, with your display name at the top. Add a logo on this card to complete the header."
            } else {
                "Preview sent to your own address. Nothing of your own is branded yet, so it carries the MissedCall Respondr wordmark and name — set a name or a logo, save, and send it again."
            };
            Ok(Json(json!({
                "sent": true,
                "to": to,
                "branded": logo_is_own || name_is_own,
                "logo_is_own": logo_is_own,
                "detail": detail
            })))
        }
        Err(detail) => Ok(Json(json!({ "sent": false, "to": to, "detail": detail }))),
    }
}

/// `GET /api/v1/branding/logo/:tenant_id` — stream an account's logo. No credential (see the module
/// docs); ids are unguessable uuids and no other tenant datum is reachable from here.
pub async fn get_logo(
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
) -> Result<Response, AppError> {
    // PUBLIC read embedded as `<img src>` in transactional mail (see the module doc): a mail client
    // holding a stale or mangled id must get the same clean 404 as an id with no row, not axum's
    // `Path<Uuid>` refusal (400 "UUID parsing failed"), which shows the recipient a parser sentence
    // where their logo should be. Same fix, same day, as `auth::handlers::get_avatar`.
    let tenant_id = Uuid::parse_str(raw_id.trim())
        .map_err(|_| AppError::NotFound("No logo for this account".into()))?;
    let row = sqlx::query("SELECT bytes, content_type FROM tenant_logos WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("No logo for this account".into()))?;

    let bytes: Vec<u8> = row.try_get("bytes")?;
    let content_type: String = row.try_get("content_type")?;

    let mut resp = Response::new(Body::from(bytes));
    let ct = header::HeaderValue::from_str(&content_type)
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream"));
    resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    // NOT cached: the URL carries a ?v= stamp, but a stale copy of a replaced logo must never be
    // served from an intermediate cache, so the endpoint itself is no-store.
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    // The stored type is pinned and the browser is told not to sniff, so a mislabelled upload can
    // never be served back as something executable.
    resp.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    Ok(resp)
}
