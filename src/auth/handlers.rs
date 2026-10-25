use axum::{
    extract::{Extension, Path, Request, State},
    http::HeaderMap,
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
    headers: HeaderMap,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    // ── Address boundary (kanban t_54b1ffab) ────────────────────────────────────────────────
    // FIRST, before any SELECT and long before any INSERT. `users.email` is both the login identity
    // and the only address the welcome/credentials mail can ever reach; the handler used to bind
    // `req.email` verbatim, so the literal string `bad` became a real account that no mail could
    // ever be delivered to. Normalises (trim + lowercase) as well as validates, and the normalised
    // value is what is checked, stored, put in the token and mailed.
    let email = email_addr::normalize(&req.email).map_err(AppError::Unprocessable)?;
    // Attribution for the workspace this route is about to mint (kanban t_4a744754, extended
    // 2026-10-17). The `X-Swift-Harness` HEADER names the harness when the caller sends one; the
    // ADDRESS is the fallback, because every domain in `crate::security::probe_addr` is fleet-
    // owned / non-routable — the same predicate that suppresses their mail — so a workspace minted
    // on one is fleet machinery by construction. Neither arm reads the request body (client data
    // the flow writes into customer-visible columns), and `None` leaves an ordinary customer's row
    // byte-identical to what it was before the column existed.
    //
    // Measured 2026-10-17: a sweep of all six app databases found 35 probe workspaces minted in
    // the previous hour with this column NULL on every one, so the only detector left was the
    // workspace NAME — which is why innocuously-named ones survived sweep after sweep.
    let probe_harness = crate::probe_harness::for_signup(&headers, &email);
    // ── Display-name boundary (measured 2026-10-17) ──────────────────────────────────────
    // This door had NO length rule while the profile screen capped at 200, and every name
    // column here is `text` — so a 4000-character pasted paragraph was stored with a 200
    // and became the customer's name, the derived workspace label and the welcome-mail
    // greeting. ONE rule now, shared with `update_profile` below.
    let name =
        crate::security::display_name::normalize(&req.name).map_err(AppError::Unprocessable)?;

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
            format!("{name}'s Workspace")
        } else {
            // The explicit label is customer-visible and was equally unbounded.
            crate::security::display_name::normalize_capped(
                a,
                crate::security::display_name::MAX_NAME_CHARS,
                "account_name",
            )
            .map_err(AppError::Unprocessable)?
        }
    };
    let ids = super::signup::create_account(
        &state,
        super::signup::NewAccount {
            email: &email,
            name: &name,
            password_hash: &password_hash,
            // The server-minted plaintext is the ONLY delivery of that password, so the
            // `welcome_credentials` template carries it (the tag door's existing posture).
            password_plain: Some(&password),
            account_name: &account_name,
            account_slug: None,
            plan_slug: "free",
            role: "account_owner",
            probe_harness: probe_harness.as_deref(),
        },
    )
    .await?;

    // The tier the visitor picked on the marketing pricing page travels WITH the signup (cron
    // 2026-10-23). Before this the page said "You picked Pro Monthly" and then posted name+email, so
    // the pick died at the door: the account landed on Free and nothing in the operator's Plan
    // Requests queue knew a paid tier had been asked for. Files the same `plan_request` ticket the
    // dashboard's upgrade button files. BEST-EFFORT: a bad slug or a DB hiccup must never refuse an
    // account that has already been minted.
    if let Err(e) = crate::handlers::tickets_handler::record_signup_plan_request(
        &state.pool,
        ids.account_id,
        &req.plan,
    )
    .await
    {
        tracing::warn!(
            "register: signup plan request '{}' could not be filed for tenant {}: {e:?}",
            req.plan,
            ids.account_id
        );
    }

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
            name: name.clone(),
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

    // A stored hash that is not a usable PHC string is NOT a credential: no password can match
    // it, so the answer is the same generic 401 a wrong password gets. It must never become a 500
    // — that both echoes the hasher's own parse message ("password hash string missing field") to
    // an unauthenticated caller and makes a broken row a probe target. WorkflowSwift's login
    // already states this rule for its placeholder rows (`'x'`, measured live).
    let valid = verify_password(&req.password, &user.password_hash).unwrap_or_default();

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

    // SESSION REVOCATION (kanban t_80787a0f): a password change/reset also ends every JWT
    // minted BEFORE it (`users.sessions_valid_from`), refused at the boundary read.
    sqlx::query(
        "UPDATE users SET password_hash = $1, sessions_valid_from = NOW(), updated_at = NOW() \
           WHERE id = $2",
    )
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
/// present, goes through the SAME guard the signup door uses (`security::display_name`): trimmed,
/// non-blank, at most 200 characters, no control characters — over-long or blank is a 422 naming the
/// field, never a 500. `username` and `company` may be CLEARED by sending an empty string, because
/// both columns are nullable; over-long values there are a 400. Answers `{"status":"ok"}` — the
/// shape the fleet profile contract names.
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

    let normalised_name = match name {
        Some(n) => {
            Some(crate::security::display_name::normalize(n).map_err(AppError::Unprocessable)?)
        }
        None => None,
    };
    for (label, v) in [("username", username), ("company", company)] {
        if let Some(v) = v {
            if v.chars().count() > MAX_FIELD {
                return Err(AppError::BadRequest(format!("{label} is too long")));
            }
        }
    }

    if let Some(n) = normalised_name {
        sqlx::query("UPDATE users SET name = $1, updated_at = NOW() WHERE id = $2")
            .bind(n)
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

/// Identify an image by its MAGIC BYTES, never by a caller-supplied content type or filename
/// (FunnelSwift t_ff948669's decision, reused here). Re-exported from the ONE shared sniffer so any
/// caller written against the old name keeps working with a single implementation behind it.
#[allow(dead_code)]
pub(crate) fn sniff_image(b: &[u8]) -> Option<&'static str> {
    crate::image_store::sniff(b)
}

/// `POST /api/v1/auth/avatar` — the image bytes, per user (card t_9cd2c8f2).
///
/// TWO body shapes, ONE handler (fleet-normalised 2026-10-10). The other five apps submit a profile
/// picture as `multipart/form-data`; this app historically took it as the RAW body, so the fleet's
/// form idiom got a 400 "Unsupported picture" here. The handler now reads BOTH: `multipart/form-data`
/// takes the first file part, anything else is the body verbatim. Either way the format is decided by
/// the bytes (never a caller-declared content type), a non-image and an empty body are refused 400,
/// and a body over [`MAX_AVATAR_BYTES`] is refused 400 as well. One row per user (upsert), so
/// re-uploading replaces the picture rather than accumulating rows. Private: caller must be signed in.
pub async fn upload_avatar(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Json<serde_json::Value>, AppError> {
    // The ONE shared image path (crate::image_store): a multipart form OR a raw image body, the
    // 2 MB envelope, the magic-byte sniff, and a part with NO filename skipped rather than mistaken
    // for the picture — so a form that posts the picture beside a caption works.
    let (content_type, body) =
        crate::image_store::read_request_image(&state, &headers, request).await?;

    sqlx::query(
        "INSERT INTO user_avatars (user_id, bytes, content_type, updated_at) VALUES ($1, $2, $3, NOW()) \
         ON CONFLICT (user_id) DO UPDATE SET bytes = EXCLUDED.bytes, \
         content_type = EXCLUDED.content_type, updated_at = NOW()",
    )
    .bind(claims.sub)
    .bind(&body)
    .bind(&content_type)
    .execute(&state.pool)
    .await?;

    Ok(Json(serde_json::json!({
        "status": "ok",
        "avatar_url": format!("/api/v1/auth/avatar/{}", claims.sub),
    })))
}

/// `DELETE /api/v1/auth/avatar` — remove the caller's OWN profile picture.
///
/// The upload twin only ever REPLACES the row (`ON CONFLICT (user_id) DO UPDATE`), so before this
/// route existed a picture could be swapped for another one but never taken down: an operator who
/// wanted their initials back had no way to ask, and both consoles could only offer "Change
/// picture". The per-account email-branding logo has had its own DELETE since it shipped
/// (`handlers::branding_handler::delete_logo`), so this closes the same gap on the profile.
/// Precedent copied, not redesigned: `FunnelSwift/src/auth/handlers.rs` @1c3ad32.
///
/// It writes the SESSION's own user — there is no path id to spoof, exactly the rule `upload_avatar`
/// follows — and it is idempotent: `removed` reports whether a row was actually there, so calling it
/// on an account with no picture answers 200 with `removed=0` rather than inventing a 404 for a
/// state that is already the state the caller asked for. `avatar_url` comes back empty so the caller
/// can repaint from the response instead of re-reading `/auth/me`.
///
/// ONE row carries the picture on this app and one statement clears it: the row in `user_avatars`
/// is both the bytes `get_avatar` streams AND the existence `me()` derives `avatar_url` from
/// (`SELECT EXISTS(SELECT 1 FROM user_avatars WHERE user_id = $1)`) — this app has NO pointer column
/// on `users`, so unlike FunnelSwift/ADASwift there is no second statement to write and no stale URL
/// that could survive. The route stays PRIVATE: `/api/v1/auth/avatar` is not in
/// `auth::route_policy::PUBLIC_ROUTES` (asserted by `the_deliberate_public_surfaces_stay_public`),
/// so an anonymous DELETE is refused at the boundary with 401 before it can reach this handler.
pub async fn delete_avatar(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    let removed = sqlx::query("DELETE FROM user_avatars WHERE user_id = $1")
        .bind(claims.sub)
        .execute(&state.pool)
        .await?
        .rows_affected();

    Ok(Json(serde_json::json!({
        "status": "ok",
        "removed": removed,
        "avatar_url": "",
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

    // The ONE serve path. NOT cached (kanban t_9cd2c8f2): the URL is stable per user, so ANY max-age
    // would keep serving the PREVIOUS face after a re-upload — measured live, a plain fetch of this
    // path right after an upload returned the old bytes (browser HTTP cache), which is exactly the
    // "byte-identical" promise the account screen depends on.
    Ok(crate::image_store::image_response(content_type, bytes))
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

    // SESSION REVOCATION (kanban t_80787a0f): a password change/reset also ends every JWT
    // minted BEFORE it (`users.sessions_valid_from`), refused at the boundary read.
    sqlx::query(
        "UPDATE users SET password_hash = $1, sessions_valid_from = NOW(), updated_at = NOW() \
           WHERE id = $2",
    )
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

// ── SIGN-IN ADDRESS CHANGE (kanban t_0d1aa081) ──────────────────────────────────────────────────
//
// Ported from the fleet's proven flow (IncentiveSwift t_6b2702e3; FunnelSwift/WorkflowSwift carried
// it first) — same two routes, same table name, the same sentences. The app-specific translation is
// only the identity table: MCR's sign-in row is `users`, so the owner key is `user_id`.
//
// Two arms, and the split is the safety property:
//   * REQUEST is authenticated and writes the SESSION's own row (no path id to spoof, the rule
//     `upload_avatar` already follows), mails a link to the NEW address, and moves nothing;
//   * CONFIRM is anonymous because a link opened from a mailbox carries no session — the token IS
//     the credential. It is listed in `auth::route_policy::PUBLIC_ROUTES`; the request twin is
//     deliberately NOT, and a test asserts both.

/// Body of `POST /api/v1/auth/email-change`.
#[derive(serde::Deserialize)]
pub struct EmailChangeInput {
    pub new_email: String,
}

/// Body of `POST /api/v1/auth/email-change/confirm`. The token IS the credential the customer just
/// received by mail.
#[derive(serde::Deserialize)]
pub struct ConfirmEmailChangeInput {
    pub token: String,
}

/// `POST /api/v1/auth/email-change` — ask to move the caller's OWN sign-in address.
///
/// Writes ONE pending `email_changes` row and mails a confirmation link to the NEW address;
/// `users.email` does not move until that link is opened. That is what stops a typo (or a hostile
/// edit by someone holding a borrowed session) from locking a customer out of their own account.
///
/// Uniqueness is checked here so a clash reads as a sentence rather than a 500 out of the
/// `users_email_key` constraint, and again at confirm time because the address can be taken in
/// between.
pub async fn request_email_change(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(body): Json<EmailChangeInput>,
) -> Result<Json<Value>, AppError> {
    // Normalises (trim + lowercase) as well as validates — the stored value must be the address
    // that can actually receive the link, the same rule `register` follows. A malformed value is a
    // 400 with the boundary's own sentence (the fleet contract this flow answers), not a 422.
    let new_email = email_addr::normalize(&body.new_email).map_err(AppError::BadRequest)?;

    let current: Option<(String, String)> =
        sqlx::query_as("SELECT email, COALESCE(name, '') FROM users WHERE id = $1")
            .bind(claims.sub)
            .fetch_optional(&state.pool)
            .await?;
    let (current_email, name) =
        current.ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

    if current_email.eq_ignore_ascii_case(&new_email) {
        return Err(AppError::BadRequest(
            "That is already your sign-in address".to_string(),
        ));
    }

    if super::signup::email_taken(&state.pool, &new_email).await? {
        return Err(AppError::BadRequest(
            "That address is already used by another account".to_string(),
        ));
    }

    let token = uuid::Uuid::new_v4().to_string();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(2);

    // One live request per user: retire the previous row before inserting the new one, so a
    // customer who mistyped once cannot redeem the older link afterwards.
    sqlx::query("UPDATE email_changes SET used = true WHERE user_id = $1 AND used = false")
        .bind(claims.sub)
        .execute(&state.pool)
        .await
        .ok();

    sqlx::query(
        "INSERT INTO email_changes (user_id, new_email, token, expires_at)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(claims.sub)
    .bind(&new_email)
    .bind(&token)
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    let app_url = crate::branding::APP_URL;
    let link = format!("{app_url}/confirm-email-change?token={token}");
    let greeting = if name.trim().is_empty() {
        "there"
    } else {
        name.trim()
    };
    let vars = serde_json::json!({
        "name": greeting,
        "new_email": new_email,
        "link": link,
        "token": token,
        "app_url": app_url,
    });
    // `claims.aid` is the tenant the account belongs to: it selects the per-account branding and
    // the account's own template row (or the app default), exactly as the welcome/reset mails do.
    if let Err(e) = crate::email::send_template_email(
        &state.pool,
        claims.aid,
        &new_email,
        "email_change",
        &vars,
    )
    .await
    {
        tracing::error!(
            "email-change: the confirmation mail could not be handed to the provider for {}: {}",
            new_email,
            e
        );
        // No link reached the customer, so the pending row must not look redeemable.
        sqlx::query("UPDATE email_changes SET used = true WHERE token = $1")
            .bind(&token)
            .execute(&state.pool)
            .await
            .ok();
        return Err(AppError::BadRequest(
            "We could not send the confirmation link, so nothing was changed. Try again in a moment."
                .to_string(),
        ));
    }

    // A reserved test address (RFC-2606, or a domain the fleet harness owns) is withheld at the
    // sender — `email::send_email_request` never hands it to a relay — so say that plainly instead
    // of telling the caller to check a mailbox that can never receive mail.
    let withheld = crate::security::probe_addr::harness_domain(&new_email).is_some();

    Ok(Json(serde_json::json!({
        "status": "pending",
        "delivered": !withheld,
        "new_email": new_email,
        "message": if withheld {
            "This address is a reserved test address, so no mail was sent. Nothing has been changed."
        } else {
            "Check the new address for a confirmation link. Your sign-in email stays the same until you open it."
        },
    })))
}

/// `POST /api/v1/auth/email-change/confirm` — anonymous by necessity: the token IS the credential
/// the customer just received, and a link opened in a browser carries no session.
///
/// Reads ONE unexpired `email_changes` row, moves that user's OWN `users.email` and nothing else,
/// and marks the row used so a forwarded link cannot be replayed.
pub async fn confirm_email_change(
    State(state): State<AppState>,
    Json(body): Json<ConfirmEmailChangeInput>,
) -> Result<Json<Value>, AppError> {
    let token = body.token.trim().to_string();
    if token.is_empty() {
        return Err(AppError::BadRequest(
            "The confirmation link is missing its code".to_string(),
        ));
    }

    let row: Option<(uuid::Uuid, uuid::Uuid, String)> = sqlx::query_as(
        "SELECT id, user_id, new_email FROM email_changes
         WHERE token = $1 AND used = false AND expires_at > NOW()",
    )
    .bind(&token)
    .fetch_optional(&state.pool)
    .await?;
    let (change_id, user_id, new_email) = row.ok_or_else(|| {
        AppError::BadRequest("That confirmation link is invalid or has expired".to_string())
    })?;

    // The address may have been taken between the request and this confirmation.
    let taken: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE lower(email) = $1 AND id <> $2 LIMIT 1")
            .bind(&new_email)
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await?;
    if taken.is_some() {
        return Err(AppError::BadRequest(
            "That address has since been used by another account, so nothing was changed"
                .to_string(),
        ));
    }

    let moved: Option<String> = sqlx::query_scalar(
        "UPDATE users SET email = $1, updated_at = NOW() WHERE id = $2 RETURNING email",
    )
    .bind(&new_email)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;
    let moved = moved.ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

    sqlx::query("UPDATE email_changes SET used = true WHERE id = $1")
        .bind(change_id)
        .execute(&state.pool)
        .await?;

    tracing::info!(
        "email-change: user {} moved its sign-in address to {}",
        user_id,
        moved
    );

    Ok(Json(serde_json::json!({
        "status": "ok",
        "email": moved,
        "message": "Your sign-in address has been changed. Use it the next time you sign in.",
    })))
}
