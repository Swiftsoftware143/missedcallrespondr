use axum::{
    body::{Body, Bytes},
    extract::{Extension, Path, State},
    http::header,
    response::Response,
    Json,
};

use serde_json::Value;
use sqlx::Row;

use super::models::{create_token, hash_password, verify_password};
use crate::{
    config::{
        AuthResponse, ChangePasswordRequest, Claims, ForgotPasswordRequest, LoginRequest,
        MeResponse, RegisterRequest, ResetPasswordRequest, TeamMember, TeamMemberResponse,
    },
    error::AppError,
    security::email_addr,
    state::AppState,
};

pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    // ── Address boundary (kanban t_54b1ffab) ────────────────────────────────────────────────
    // FIRST, before any SELECT and long before any INSERT. `users.email` is both the login identity
    // and the only address the welcome/credentials mail can ever reach; the handler used to bind
    // `req.email` verbatim, so the literal string `bad` became a real account that no mail could
    // ever be delivered to. Normalises (trim + lowercase) as well as validates, and the normalised
    // value is what is checked, stored, put in the token and mailed.
    let email = email_addr::normalize(&req.email).map_err(AppError::Unprocessable)?;

    // The duplicate check runs FIRST, before the Argon2 work: `register` is reachable without a
    // credential, and a hash nobody will store must not be computed for a duplicate (the shared
    // core re-checks before its first write, so this is the cheap first gate, not the only one).
    if super::signup::email_taken(&state.pool, &email).await? {
        return Err(AppError::Conflict(
            "A user with this email already exists. Try signing in.".into(),
        ));
    }

    // David's signup model (as IncentiveSwift/FunnelSwift ship): the page collects NAME + EMAIL
    // only, so `password` may arrive empty. The server then mints one and mails it; the user
    // confirms their address by signing in with it. A caller that still supplies one is honoured.
    let password = if req.password.is_empty() {
        super::signup::generate_temp_password()
    } else {
        req.password.clone()
    };

    let password_hash = hash_password(&password).map_err(|e| AppError::Internal(e.to_string()))?;

    // ── The mint, through the ONE shared writer (kanban t_1d08bd9a, design §3.1 rule 4) ─────────
    // `auth::signup::create_account` is the same function the fleet-internal tag door
    // (`POST /api/v1/internal/provision-free-account`) calls, so the account this public signup
    // mints and the account a FunnelSwift tag mints cannot drift: one `tenants` row, one
    // `account_owner` `users` row, one `tenant_plans` row on `free` with 50 starter credits.
    // `account_slug: None` makes `create_account` DERIVE a not-taken slug through
    // `signup::unique_account_slug` (kanban t_1a26f923 — the raw name derivation used to collide on
    // `tenants_slug_key` and 500 the second visitor of the same name); `password_plain` now
    // carries the server-minted first password so the `welcome_credentials` mail can deliver it.
    // No workspace-name field on the page: derive "<name>'s Workspace" when none was supplied.
    let account_name = {
        let a = req.account_name.trim();
        if a.is_empty() {
            format!("{}'s Workspace", req.name)
        } else {
            a.to_string()
        }
    };
    let ids = super::signup::create_account(
        &state,
        super::signup::NewAccount {
            email: &email,
            name: &req.name,
            password_hash: &password_hash,
            // The server-minted plaintext is the ONLY delivery of that password, so the
            // `welcome_credentials` template carries it (the tag door's existing posture).
            password_plain: Some(&password),
            account_name: &account_name,
            account_slug: None,
            plan_slug: "free",
            role: "account_owner",
        },
    )
    .await?;

    let claims = Claims {
        sub: ids.user_id,
        email: email.clone(),
        aid: ids.account_id,
        role: "account_owner".into(),
        exp: (chrono::Utc::now().timestamp() + 86400 * 7) as usize,
        iat: chrono::Utc::now().timestamp() as usize,
    };

    let token = create_token(&claims, &state.config.jwt_secret)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    // NOTE: the legacy signup -> CoreSwift "cross-app/tag-sync" push was removed here.
    // It was provably dead: CoreSwift's handler requires `lead.id` (TagSyncLead.id is a
    // non-optional String) while this payload only ever sent name+email, so every signup
    // got HTTP 422 and no hub row was ever written. It also authenticated with the
    // fleet-wide INTERNAL_SYNC_KEY env secret, which the platform standard forbids.
    // The live inbound hub path is handlers::coreswift_external::push_lead_to_coreswift
    // (tenant BYOK csk_ key), fired on real captures - not on signup.

    Ok(Json(AuthResponse {
        token,
        team_member: TeamMemberResponse {
            id: ids.user_id,
            email,
            name: req.name,
            tenant_id: ids.account_id,
            role: "account_owner".into(),
        },
    }))
}

pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    // The same normalisation `register` stores by, matched case-insensitively so an account stored
    // with capitals (or created before normalisation existed — live has `Zaarhub@gmail.com`) still
    // resolves when the customer retypes their address with different casing. A malformed value is
    // NOT refused here: login answers its own 401 for every wrong credential, and it must not become
    // an account-existence oracle. It simply matches nothing.
    let user = sqlx::query_as::<_, TeamMember>("SELECT * FROM users WHERE lower(email) = $1")
        .bind(email_addr::lookup_key(&req.email))
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::Unauthorized("Invalid email or password".into()))?;

    let valid = verify_password(&req.password, &user.password_hash)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if !valid {
        return Err(AppError::Unauthorized("Invalid email or password".into()));
    }

    let claims = Claims {
        sub: user.id,
        email: user.email.clone(),
        aid: user.tenant_id,
        role: user.role.clone(),
        exp: (chrono::Utc::now().timestamp() + 86400 * 7) as usize,
        iat: chrono::Utc::now().timestamp() as usize,
    };

    let token = create_token(&claims, &state.config.jwt_secret)
        .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok(Json(AuthResponse {
        token,
        team_member: user.into(),
    }))
}

/// The real plan tier name for a tenant (programme t_2cb77960, card t_9cd2c8f2).
///
/// `tenant_plans` holds at most one row per tenant (000020) and joins `plans` for the display name.
/// A tenant with no row — or a tournament of rows where none is active — falls back to "Free", the
/// same default the console's own Free posture already uses: a label the account can always read,
/// never a blank and never the literal word "User". A database hiccup is NOT fatal here (this is a
/// display field, not an authorization decision), so it degrades to the default rather than 500-ing
/// the whole account screen.
async fn plan_name_for(pool: &sqlx::PgPool, tenant_id: uuid::Uuid) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT p.name FROM tenant_plans tp JOIN plans p ON p.id = tp.plan_id \
         WHERE tp.tenant_id = $1 AND tp.status = 'active' \
         ORDER BY tp.created_at DESC LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| "Free".to_string())
}

/// `GET /api/v1/auth/me` — the signed-in account, INCLUDING the real plan tier (card t_9cd2c8f2).
///
/// Before this card the route answered the raw `TeamMemberResponse`, which names no tier, so the
/// console could only print a guess for the level label (the FunnelSwift defect: it printed the
/// role word "User"). This answer carries `plan_name` from `tenant_plans`, the optional `company`
/// and `username` the profile screen edits, and `avatar_url` when a picture exists.
pub async fn me(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<MeResponse>, AppError> {
    let row = sqlx::query(
        "SELECT id, email, name, username, company, tenant_id, role FROM users WHERE id = $1",
    )
    .bind(claims.sub)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    let tenant_id: uuid::Uuid = row.try_get("tenant_id")?;
    let plan_name = plan_name_for(&state.pool, tenant_id).await;

    let has_avatar: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_avatars WHERE user_id = $1)")
            .bind(claims.sub)
            .fetch_one(&state.pool)
            .await?;

    Ok(Json(MeResponse {
        id: row.try_get("id")?,
        email: row.try_get("email")?,
        name: row.try_get("name")?,
        username: row.try_get("username")?,
        company: row.try_get("company")?,
        tenant_id,
        role: row.try_get("role")?,
        plan_name,
        avatar_url: if has_avatar {
            Some(format!("/api/v1/auth/avatar/{}", claims.sub))
        } else {
            None
        },
    }))
}

pub async fn change_password(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    if req.new_password.len() < 8 {
        return Err(AppError::BadRequest(
            "New password must be at least 8 characters".into(),
        ));
    }

    let user = sqlx::query_as::<_, TeamMember>("SELECT * FROM users WHERE id = $1")
        .bind(claims.sub)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::Unauthorized("User not found".into()))?;

    let valid = verify_password(&req.current_password, &user.password_hash)
        .map_err(|e| AppError::Internal(e.to_string()))?;
    if !valid {
        return Err(AppError::Unauthorized(
            "Current password is incorrect".into(),
        ));
    }

    let new_hash =
        hash_password(&req.new_password).map_err(|e| AppError::Internal(e.to_string()))?;

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(&new_hash)
        .bind(user.id)
        .execute(&state.pool)
        .await?;

    Ok(Json(
        serde_json::json!({"message": "Password updated successfully"}),
    ))
}

/// `PUT /api/v1/auth/profile` — `{name?, username?, company?}` (programme t_2cb77960, card
/// t_9cd2c8f2).
///
/// The rules, in the words the console depends on. An ABSENT key leaves the stored value untouched,
/// so a save that only sends `name` cannot blank a company the account already set. `name`, when
/// present, must be non-blank (400) — it is the account's display name. `username` and `company`
/// may be CLEARED by sending an empty string, because both columns are nullable. Over-long values
/// are a 400, never a 500. Answers `{"status":"ok"}` — the shape the fleet profile contract names.
pub async fn update_profile(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    /// Longest value any of the three columns accepts from the form. Over-long is a 400, never a 500.
    const MAX_FIELD: usize = 200;

    let name = req.get("name").and_then(|v| v.as_str());
    let username = req.get("username").and_then(|v| v.as_str());
    let company = req.get("company").and_then(|v| v.as_str());

    if let Some(n) = name {
        if n.trim().is_empty() {
            return Err(AppError::BadRequest("name cannot be empty".into()));
        }
    }
    for (label, v) in [("name", name), ("username", username), ("company", company)] {
        if let Some(v) = v {
            if v.chars().count() > MAX_FIELD {
                return Err(AppError::BadRequest(format!("{label} is too long")));
            }
        }
    }

    if let Some(n) = name {
        sqlx::query("UPDATE users SET name = $1, updated_at = NOW() WHERE id = $2")
            .bind(n.trim())
            .bind(claims.sub)
            .execute(&state.pool)
            .await?;
    }
    if let Some(u) = username {
        let val: Option<&str> = if u.trim().is_empty() {
            None
        } else {
            Some(u.trim())
        };
        sqlx::query("UPDATE users SET username = $1, updated_at = NOW() WHERE id = $2")
            .bind(val)
            .bind(claims.sub)
            .execute(&state.pool)
            .await?;
    }
    if let Some(c) = company {
        let val: Option<&str> = if c.trim().is_empty() {
            None
        } else {
            Some(c.trim())
        };
        sqlx::query("UPDATE users SET company = $1, updated_at = NOW() WHERE id = $2")
            .bind(val)
            .bind(claims.sub)
            .execute(&state.pool)
            .await?;
    }

    Ok(Json(serde_json::json!({"status": "ok"})))
}

/// Largest profile picture this route accepts. The contract says 2 MB; the route's
/// `DefaultBodyLimit` is a byte or two above this so a body that is OVER the cap is refused by
/// `upload_avatar` with this app's JSON 400, not by axum with an unreadable text/plain 413.
pub const MAX_AVATAR_BYTES: usize = 2 * 1024 * 1024;

/// Identify an image by its MAGIC BYTES, never by a caller-supplied content type or filename
/// (FunnelSwift t_ff948669's decision, reused here). Returns the content type to store, or `None`
/// for anything that is not one of the four accepted formats.
pub(crate) fn sniff_image(b: &[u8]) -> Option<&'static str> {
    if b.len() >= 8 && b.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if b.len() >= 3 && b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if b.len() >= 6 && (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        return Some("image/gif");
    }
    if b.len() >= 12 && b.starts_with(b"RIFF") && &b[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// `POST /api/v1/auth/avatar` — the raw image bytes, per user (card t_9cd2c8f2).
///
/// The body IS the picture: no multipart envelope, no filename, no caller-declared content type is
/// trusted. The format is decided by the bytes, a non-image and an empty body are refused 400, and a
/// body over [`MAX_AVATAR_BYTES`] is refused 400 as well. One row per user (upsert), so re-uploading
/// replaces the picture rather than accumulating rows. Private: the caller must present their session.
pub async fn upload_avatar(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    if body.is_empty() {
        return Err(AppError::BadRequest("No picture data received".into()));
    }
    if body.len() > MAX_AVATAR_BYTES {
        return Err(AppError::BadRequest(
            "Profile picture must be 2 MB or smaller".into(),
        ));
    }
    let content_type = sniff_image(&body).ok_or_else(|| {
        AppError::BadRequest("Unsupported picture — use a PNG, JPEG, GIF or WebP image".into())
    })?;

    sqlx::query(
        "INSERT INTO user_avatars (user_id, bytes, content_type, updated_at) VALUES ($1, $2, $3, NOW()) \
         ON CONFLICT (user_id) DO UPDATE SET bytes = EXCLUDED.bytes, \
         content_type = EXCLUDED.content_type, updated_at = NOW()",
    )
    .bind(claims.sub)
    .bind(body.as_ref())
    .bind(content_type)
    .execute(&state.pool)
    .await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "avatar_url": format!("/api/v1/auth/avatar/{}", claims.sub),
    })))
}

/// `GET /api/v1/auth/avatar/:user_id` — serve the stored picture (card t_9cd2c8f2).
///
/// ANONYMOUS BY CONSTRUCTION and narrow by design: an `<img src>` cannot carry a bearer token, so
/// the route is on `route_policy::PUBLIC_ROUTES`. It returns one thing — the bytes one user
/// uploaded, keyed by an unguessable uuid, under the content type sniffed at upload time. No tenant
/// column, no credential, no row of user data; a user with no picture answers 404. The authenticated
/// upload twin stays private.
pub async fn get_avatar(
    State(state): State<AppState>,
    Path(user_id): Path<uuid::Uuid>,
) -> Result<Response, AppError> {
    let row = sqlx::query("SELECT bytes, content_type FROM user_avatars WHERE user_id = $1")
        .bind(user_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("No picture for this account".into()))?;

    let bytes: Vec<u8> = row.try_get("bytes")?;
    let content_type: String = row.try_get("content_type")?;

    let mut resp = Response::new(Body::from(bytes));
    let ct = header::HeaderValue::from_str(&content_type)
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream"));
    resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    // NOT cached (kanban t_9cd2c8f2). The URL is stable per user, so ANY max-age would keep serving
    // the PREVIOUS face after a re-upload — measured live: a plain fetch of this path right after an
    // upload returned the old bytes (browser HTTP cache), which is exactly the "byte-identical"
    // promise the account screen depends on. The console adds its own cache-buster to the <img>; the
    // endpoint itself must always answer with the CURRENT bytes.
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    Ok(resp)
}

pub async fn forgot_password(
    State(state): State<AppState>,
    Json(req): Json<ForgotPasswordRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // The SAME boundary rule as `register`, from the same function: an address that could never
    // receive the reset mail is refused with the same 422/field shape instead of silently
    // reporting "if the email exists…" for an address that cannot exist as a mailbox. This reply
    // is unconditional for every well-formed address, so it still leaks nothing about accounts.
    let email = email_addr::normalize(&req.email).map_err(AppError::Unprocessable)?;

    if let Some(user) =
        sqlx::query_as::<_, TeamMember>("SELECT * FROM users WHERE lower(email) = $1")
            .bind(&email)
            .fetch_optional(&state.pool)
            .await?
    {
        let token = uuid::Uuid::new_v4().to_string();
        let expires_at = chrono::Utc::now() + chrono::Duration::hours(24);

        sqlx::query("UPDATE password_resets SET used = true WHERE user_id = $1 AND used = false")
            .bind(user.id)
            .execute(&state.pool)
            .await
            .ok();

        sqlx::query("INSERT INTO password_resets (user_id, token, expires_at) VALUES ($1, $2, $3)")
            .bind(user.id)
            .bind(&token)
            .bind(expires_at)
            .execute(&state.pool)
            .await?;

        // Send password reset email via template system
        let vars = serde_json::json!({
            "name": user.name,
            "token": token,
            "app_url": "https://app.missedcallrespondr.com",
        });
        // kanban t_feab8aff: the mail is the ACCOUNT's (the user belongs to `user.tenant_id`), so
        // the tenant id is passed through — the per-tenant email branding (and any tenant template
        // override) is resolved from it. It used to be `Uuid::nil()`, which made every reset mail
        // fall back to the system default and silently ignored the account's own identity.
        match crate::email::send_template_email(
            &state.pool,
            user.tenant_id,
            &user.email,
            "password_reset",
            &vars,
        )
        .await
        {
            Ok(_) => tracing::info!("Password reset email sent to {}", user.email),
            Err(e) => tracing::error!(
                "Failed to send password reset email to {}: {}",
                user.email,
                e
            ),
        }
    }

    Ok(Json(
        serde_json::json!({"message": "If the email exists, a password reset link has been sent"}),
    ))
}

pub async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    if req.new_password.len() < 8 {
        return Err(AppError::BadRequest(
            "New password must be at least 8 characters".into(),
        ));
    }

    use sqlx::Row;
    let reset = sqlx::query(
        "SELECT id, user_id FROM password_resets WHERE token = $1 AND used = false AND expires_at > NOW()",
    )
    .bind(&req.token)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::BadRequest("Invalid or expired reset token".into()))?;

    let reset_id: uuid::Uuid = reset.get("id");
    let user_id: uuid::Uuid = reset.get("user_id");

    let new_hash =
        hash_password(&req.new_password).map_err(|e| AppError::Internal(e.to_string()))?;

    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(&new_hash)
        .bind(user_id)
        .execute(&state.pool)
        .await?;

    sqlx::query("UPDATE password_resets SET used = true WHERE id = $1")
        .bind(reset_id)
        .execute(&state.pool)
        .await?;

    Ok(Json(
        serde_json::json!({"message": "Password has been reset successfully"}),
    ))
}

pub async fn get_usage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Value>, AppError> {
    let tid = claims.aid;
    let usage = crate::features::get_usage_json(&state.pool, tid).await;
    Ok(Json(usage))
}
