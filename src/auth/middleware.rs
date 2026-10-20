use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
    Json,
};
use serde_json::json;

use super::models::validate_token;
use crate::state::AppState;

/// Platform-admin roles — the UNION of what this app's own admin handlers already accept, so the
/// class guard can never be stricter than the handlers it fronts:
///   * `admin_handler::list_all_tenants`  accepts `admin | super_admin | agency_admin`
///   * `admin_handler::impersonate`       accepted `agency_admin` (initial commit f515e9e)
///   * `checkout_handler::upsert/delete_payment_provider` accept `super_admin`
///
/// Live `users.role` census at the time of the fix (kanban t_92ec2f21): 2 x `admin`, 23 x
/// `account_owner` — and nothing else.
///
/// Deliberately NOT platform admins:
///   * `company_admin` — minted for a TENANT's own user by `admin_handler::portfolio_sync`
///   * `impersonated`  — the token `impersonate` hands out; if it counted as an admin, an
///     impersonated customer session could escalate back into the whole admin surface
pub fn is_platform_admin(role: &str) -> bool {
    matches!(role, "admin" | "super_admin" | "agency_admin")
}

/// Does a token's platform-admin CLAIM exceed the authority its `users` row still grants?
/// (kanban t_28184826)
///
/// Measured live on funnelswift 2026-10-20 after kanban t_53fcb666 shipped: the boundary proved the
/// row EXISTED, but [`admin_surface_denied`] still decided on `claims.role`. So DEMOTING an operator
/// (the other half of "delete or disable the account") left the token holding the platform surface
/// for its whole life. A demotion is an `UPDATE users SET role=…`; it has to take effect on the next
/// request, not at token expiry.
///
/// One-directional on purpose: a claim LOWER than the row is not this rule's business and every
/// tenant surface keeps the behaviour it had. The synthetic roles this app mints are naturally
/// skipped — `impersonated` and `company_admin` are not platform admins in [`is_platform_admin`], so
/// neither can ever be "more" than a row.
pub fn platform_admin_claim_outranks_row(claim_role: &str, row_role: &str) -> bool {
    is_platform_admin(claim_role) && !is_platform_admin(row_role)
}

/// The one user-visible string for a credential whose authority the live row no longer backs
/// (kanban t_28184826). Same wording as the rest of the fleet, so the SPA's 401 arm turns it into
/// "Session expired" + sign-out rather than a generic error toast.
pub const SESSION_REVOKED: &str = "This session is no longer valid. Please sign in again.";

/// The platform-admin surface: the `/api/v1/admin/*` prefix this card is about, plus the
/// `/api/v1/payment-providers` family — same defect, same class (platform-global payment config
/// with NO tenant face): `routes.rs` labels it "Payment Providers (admin)" and the only caller on
/// the box is the admin panel. Census before folding it in: `grep -rl payment-providers` over the
/// three served roots hits `www-admin/missedcall/index.html` only — the tenant app (`www-app`) and
/// the public checkout pages reference it ZERO times (`POST /api/v1/checkout/create` reads the
/// providers from the database server-side).
///
/// Matched by path SEGMENT, never by bare string prefix: `/api/v1/administrators` and
/// `/api/v1/payment-providers-public` must stay outside the surface.
fn is_admin_surface(path: &str) -> bool {
    // `/api/v1/email-templates` is the third member of this class, measured 2026-10-17: the
    // handlers behind it take NO tenant context at all (`aid IS NULL` on every row they create,
    // and `list` selects the whole table), i.e. they are the PLATFORM's email wording — the same
    // platform-global surface as the two above. It sat on the ordinary protected router, so a
    // tenant token could read, create, update and delete the app-wide templates, including the
    // welcome mail that carries a new customer's password (probe: a `team_member` token answered
    // `GET /api/v1/email-templates` 200 with the 3 rows and `POST` 200 with a new row, while
    // `GET /api/v1/admin/tenants` correctly answered 403). The tenant app never calls it
    // (`grep -rl email-templates` over `www-app/missedcall` and `www/missedcall` = no hits), and
    // CoreSwift-CRM's own module documents the same route family as "platform admin only", so
    // closing it here leaves no tenant-facing caller behind.
    const ADMIN_SURFACE: &[&str] = &[
        "/api/v1/admin",
        "/api/v1/payment-providers",
        "/api/v1/email-templates",
    ];
    ADMIN_SURFACE.iter().any(|base| {
        path.strip_prefix(base)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

/// True when the caller is authenticated but not authorised for the platform-admin surface.
/// A missing/garbage token is a different answer (401) and is decided earlier in the middleware.
pub fn admin_surface_denied(path: &str, role: &str) -> bool {
    is_admin_surface(path) && !is_platform_admin(role)
}

/// The ONE operator path an impersonation session must be able to reach: the route whose entire
/// purpose is to end that session.
pub const ADMIN_STOP_IMPERSONATION: &str = "/api/v1/admin/stop-impersonation";

/// This app's impersonation marker (kanban t_ff1e3bb1). `handlers::admin_handler::impersonate`
/// mints the session token FOR the impersonated account with the LITERAL role `impersonated` and no
/// separate claim, so the role itself is the ready-made key here (see `is_platform_admin` above,
/// which deliberately refuses it). A future mint that switches to a boolean claim only has to change
/// this one function and its call site.
pub fn is_impersonated_session(role: &str) -> bool {
    role == "impersonated"
}

/// True when `path` is the one operator route an impersonation session must be able to call.
/// Exact match only: a trailing slash, a deeper segment or a lookalike must NOT ride in on it.
pub fn is_stop_impersonation(path: &str) -> bool {
    path == ADMIN_STOP_IMPERSONATION
}

/// The full platform-admin-surface decision, including the single carve-out an impersonation
/// session needs: `true` means answer 403.
///
/// [`is_admin_surface`] stays a PURE path test and [`admin_surface_denied`] stays its pure
/// path+role answer; this is the documented decision the middleware makes. Measured live on
/// admin.missedcallrespondr.com before this (kanban t_ff1e3bb1, probe
/// `/opt/swift/fleet/sibling-stop-impersonation-probe.py`): the impersonation session's own Stop
/// answered `403 {"error":"Platform admin role required","role":"impersonated"}` from the
/// middleware, before `stop_impersonation` ever ran — the session could not end itself.
///
/// The exception is exactly ONE path AND it requires the impersonation marker, so a plain tenant
/// token (`account_owner` / `company_admin`) gains nothing from it, no sibling admin route rides in
/// on the prefix, and the impersonation token still reaches nothing else on the surface.
pub fn admin_surface_denied_for(path: &str, role: &str, impersonating: bool) -> bool {
    if impersonating && is_stop_impersonation(path) {
        return false;
    }
    admin_surface_denied(path, role)
}

pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Missing authorization header"})),
            )
        })?;

    let token = auth_header.strip_prefix("Bearer ").ok_or_else(|| {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid authorization format"})),
        )
    })?;

    let claims = validate_token(token, &state.config.jwt_secret).map_err(|_| {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "Invalid or expired token"})),
        )
    })?;

    // The class gate (kanban t_92ec2f21): authenticated is not authorised. Everything under
    // `/api/v1/admin/*` (and the payment-provider config) is platform-admin only, decided HERE —
    // at the choke point every protected route already passes through — instead of per handler,
    // so a route added to the admin surface later is closed by default rather than open by default.
    // 403, not 401: the caller's credential is valid, it just carries the wrong role.
    //
    // The impersonation marker is read from the ROLE, which is the key this app's mint uses
    // (`admin_handler::impersonate` writes the literal `impersonated`), and it buys exactly one
    // route: the one that ends the session (kanban t_ff1e3bb1 — before this the session's own Stop
    // was refused here, so it could only be abandoned by dropping the token).
    let impersonating = is_impersonated_session(&claims.role);
    if admin_surface_denied_for(req.uri().path(), &claims.role, impersonating) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "Platform admin role required",
                "required_role": "admin",
                "role": claims.role,
            })),
        ));
    }

    // ── the USER behind the credential must still exist (kanban t_53fcb666) ───────────────────
    // A signature only proves the token was minted here. Everything below — and every handler
    // behind it — authorises from the claims alone, so deleting an account left a working
    // credential for its whole life (measured on a sibling app 2026-10-20: /auth/me and a
    // platform-admin read both answered 200 after the user + tenant rows were deleted). Deleting
    // the row is how the fleet remediates a compromised account AND how its probe sweep cleans up
    // throwaways, so deletion has to BE the revocation. One primary-key read, for every role.
    //
    // `users` here has no `is_active` column, so existence is the whole test. Fail closed: a
    // credential naming nobody, and a database that cannot answer, both refuse — 401 and 503
    // respectively, never a pass.
    //
    // The same read now also answers the DEMOTION half (kanban t_28184826): it returns the row's
    // live `role`, and a credential that still CLAIMS platform-admin authority for a row that no
    // longer grants it is refused exactly like a deleted account. Before this the admin decision
    // read `claims.role` alone, so demoting an operator left their token holding the surface.
    match user_role(&state.pool, claims.sub).await {
        Ok(Some((row_role, valid_from))) => {
            if platform_admin_claim_outranks_row(&claims.role, &row_role) {
                tracing::warn!(
                    "refused a platform-admin claim the users row no longer grants (user {})",
                    claims.sub
                );
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": SESSION_REVOKED})),
                ));
            }
            // SESSION REVOCATION (kanban t_80787a0f): the same read returns the instant this
            // account last changed or reset its password, so a token minted before it stops here —
            // 401, exactly like a deleted account. NULL (never rotated) changes nothing.
            if session_predates_password_change(Some(claims.iat as i64), valid_from) {
                tracing::warn!(
                    "refused a session minted before the account's password change (user {})",
                    claims.sub
                );
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(json!({"error": SESSION_REVOKED})),
                ));
            }
        }
        Ok(None) => {
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "Invalid or expired token"})),
            ));
        }
        Err(e) => {
            tracing::error!("user existence lookup failed for {}: {e}", claims.sub);
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "Authentication temporarily unavailable"})),
            ));
        }
    }

    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

/// What the `users` row named by a token's `sub` still grants: its live `role` and the instant its
/// sessions were last ended by a password change, or `None` when the account no longer exists.
///
/// The USER half of the boundary (kanban t_53fcb666) plus the DEMOTION half (kanban t_28184826):
/// this app's `users` table carries no `is_active`, so a row that exists is a live account — but its
/// `role` is the authority the row grants TODAY, which the caller compares against the token's
/// claim.
///
/// The second field is `users.sessions_valid_from` as a unix epoch (`None` = this account never
/// changed or reset its password, so no session is restricted); a token whose `iat` predates it is
/// refused, which is what makes a password change revoke the sessions that came before it (kanban
/// t_80787a0f).
async fn user_role(
    db: &sqlx::PgPool,
    user_id: uuid::Uuid,
) -> Result<Option<(String, Option<i64>)>, sqlx::Error> {
    sqlx::query_as::<_, (String, Option<i64>)>(
        "SELECT role, floor(EXTRACT(EPOCH FROM sessions_valid_from))::bigint
           FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
}

/// Does a session minted at `jwt_iat` predate the instant its account last changed or reset its
/// password? (kanban t_80787a0f)
///
/// Pure, so both arms are unit-tested. `valid_from` is `users.sessions_valid_from` as a unix epoch
/// (`None` = never restricted, which is every account until it rotates its password, so every
/// token already in the wild keeps working). A session with no `iat` cannot prove it is newer than
/// the change, and the safe answer for a credential is "cannot prove → refuse". `iat` is second-granularity (and the change's own timestamp is floored to a second for this reason), so a session minted in the SAME second as the change cannot be told apart from a re-login and is allowed.
pub fn session_predates_password_change(jwt_iat: Option<i64>, valid_from: Option<i64>) -> bool {
    match valid_from {
        None => false,
        Some(v) => match jwt_iat {
            None => true,
            Some(iat) => iat < v,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        admin_surface_denied, admin_surface_denied_for, is_impersonated_session, is_platform_admin,
        is_stop_impersonation, platform_admin_claim_outranks_row, session_predates_password_change,
        SESSION_REVOKED,
    };

    /// The reported defect (kanban t_28184826): the boundary proved the row existed, but the admin
    /// decision still read `claims.role`, so DEMOTING an operator left the token holding the
    /// platform surface (admin/tenants, payment providers, platform email templates) for its whole
    /// life. A claim of platform authority the row no longer grants must be refused, while a claim
    /// LOWER than the row — and this app's synthetic `impersonated` role — stay exactly as they are.
    #[test]
    fn a_platform_admin_claim_the_row_no_longer_grants_is_refused() {
        for demoted_to in [
            "account_owner",
            "company_admin",
            "business_user",
            "impersonated",
            "",
        ] {
            for claim in ["admin", "super_admin", "agency_admin"] {
                assert!(
                    platform_admin_claim_outranks_row(claim, demoted_to),
                    "a {claim} claim over a {demoted_to:?} row must be refused"
                );
            }
        }
        // A real platform admin is untouched.
        for claim in ["admin", "super_admin", "agency_admin"] {
            for row in ["admin", "super_admin", "agency_admin"] {
                assert!(!platform_admin_claim_outranks_row(claim, row));
            }
        }
        // One-directional: a lower claim never trips it, so tenant surfaces keep their behaviour.
        assert!(!platform_admin_claim_outranks_row("account_owner", "admin"));
        assert!(!platform_admin_claim_outranks_row("impersonated", "admin"));
        assert!(!platform_admin_claim_outranks_row(
            "company_admin",
            "account_owner"
        ));
        // The refusal is the fleet's "session expired" string, which the SPA matches.
        assert!(SESSION_REVOKED.contains("sign in again"));
    }

    #[test]
    fn platform_admin_roles_are_the_union_the_handlers_accept() {
        for role in ["admin", "super_admin", "agency_admin"] {
            assert!(is_platform_admin(role), "{role} is a platform admin");
        }
        // A TENANT's own admin, the impersonation token, a plain tenant user and a role the
        // database has never seen must all be refused.
        for role in [
            "account_owner",
            "company_admin",
            "impersonated",
            "user",
            "owner",
            "ADMIN",
            "",
        ] {
            assert!(
                !is_platform_admin(role),
                "{role} must NOT be a platform admin"
            );
        }
    }

    #[test]
    fn the_admin_surface_is_a_path_segment_not_a_string_prefix() {
        // every admin route the served panel calls, in both directions
        for path in [
            "/api/v1/admin",
            "/api/v1/admin/plans",
            "/api/v1/admin/plans/7b0f/features",
            "/api/v1/admin/tenants/7b0f/credits",
            "/api/v1/admin/tenants/7b0f",
            "/api/v1/admin/impersonate",
            "/api/v1/admin/stop-impersonation",
            "/api/v1/admin/portfolio-sync",
            "/api/v1/admin/telnyx-config",
            "/api/v1/admin/site",
            "/api/v1/payment-providers",
            "/api/v1/payment-providers/stripe",
            // the payment-key CHECK (kanban t_7bdfcc4a): three segments, and it reaches the
            // provider with a credential, so it must be inside the surface, not beside it.
            "/api/v1/payment-providers/stripe/test",
            // the platform email-template family (2026-10-17): global rows, no tenant context
            "/api/v1/email-templates",
            "/api/v1/email-templates/7b0f",
        ] {
            assert!(
                admin_surface_denied(path, "account_owner"),
                "{path} must refuse account_owner"
            );
            assert!(
                !admin_surface_denied(path, "admin"),
                "{path} must accept admin"
            );
        }
        // lookalikes and ordinary tenant routes stay open
        for path in [
            "/api/v1/administrators",
            "/api/v1/payment-providers-public",
            // a lookalike of the newly-closed family must NOT ride in on the prefix
            "/api/v1/email-templates-public",
            "/api/v1/contacts",
            "/api/v1/me/usage",
            "/api/v1/auth/me",
            "/api/v1/checkout/create",
            // The tenant-facing plan catalogue (kanban t_a57ae7db) the plan-and-billing screen
            // reads: it sits NEXT TO the platform-admin `/api/v1/admin/plans` in the router, so it
            // is pinned here to keep a future widening of the admin prefix from swallowing it.
            "/api/v1/plans",
        ] {
            assert!(
                !admin_surface_denied(path, "account_owner"),
                "{path} is not the admin surface"
            );
        }
    }

    /// The live defect (kanban t_ff1e3bb1): an impersonation session could START but not STOP.
    /// `admin_handler::impersonate` mints the session token with the literal role `impersonated`,
    /// never a platform-admin role, so the class gate refused every `/api/v1/admin/*` path to it —
    /// `stop-impersonation` included, before its handler ran. Both directions of the one carve-out,
    /// the non-impersonation arms and the near-miss paths are pinned here so a future widening has
    /// to change this test too.
    #[test]
    fn an_impersonation_session_may_end_itself_and_nothing_else_on_the_admin_surface() {
        let stop = "/api/v1/admin/stop-impersonation";
        // this app's marker, both ways
        assert!(is_impersonated_session("impersonated"));
        for role in ["account_owner", "company_admin", "admin", "super_admin", ""] {
            assert!(
                !is_impersonated_session(role),
                "{role} is not an impersonation session"
            );
        }
        assert!(is_stop_impersonation(stop));

        // The one carve-out: the impersonation session reaches the route that ends it...
        assert!(!admin_surface_denied_for(stop, "impersonated", true));
        // ...and still reaches NOTHING else on the surface — the prefix is not a door.
        for path in [
            "/api/v1/admin",
            "/api/v1/admin/impersonate",
            "/api/v1/admin/tenants",
            "/api/v1/admin/tenants/bulk-delete",
            "/api/v1/admin/plans",
            "/api/v1/admin/portfolio-sync",
            "/api/v1/admin/telnyx-config",
            "/api/v1/payment-providers",
        ] {
            assert!(
                admin_surface_denied_for(path, "impersonated", true),
                "an impersonation session must not reach {path}"
            );
        }

        // Near-miss paths must NOT ride in on the carve-out: exact match only.
        for path in [
            "/api/v1/admin/stop-impersonation/",
            "/api/v1/admin/stop-impersonation/anything",
            "/api/v1/admin/stop-impersonationx",
            "/api/v1/admin/stop-impersonate",
        ] {
            assert!(!is_stop_impersonation(path), "{path} is not the stop route");
            assert!(
                admin_surface_denied_for(path, "impersonated", true),
                "{path} must NOT ride the stop-impersonation carve-out"
            );
        }
        // An impersonation-shaped path OUTSIDE the surface is not the carve-out either, and it is
        // not on the surface at all — the router answers it, the gate never sees a door here.
        assert!(!is_stop_impersonation("/api/v1/stop-impersonation"));
        assert!(!admin_surface_denied_for(
            "/api/v1/stop-impersonation",
            "impersonated",
            true
        ));

        // A TENANT's OWN role strings gain nothing — that is the whole point of keying on the
        // impersonation marker and not on the path alone. These must stay refused.
        for role in ["account_owner", "company_admin"] {
            assert!(
                admin_surface_denied_for(stop, role, false),
                "{role} must still be refused on the stop route"
            );
        }
        // The marker is what the decision reads, and in the middleware it is DERIVED from the role
        // (`is_impersonated_session`): a tenant role can never present it, so the pair above is the
        // live shape. A token carrying the impersonated role with no marker is refused too.
        assert!(admin_surface_denied_for(stop, "impersonated", false));

        // The platform admin keeps every door, marker or not.
        for role in ["admin", "super_admin", "agency_admin"] {
            assert!(!admin_surface_denied_for(stop, role, false));
            assert!(!admin_surface_denied_for(stop, role, true));
            assert!(!admin_surface_denied_for(
                "/api/v1/admin/tenants",
                role,
                false
            ));
        }
    }

    /// The reported defect (kanban t_80787a0f): changing or resetting the password revoked
    /// nothing. The rule is one comparison — a session minted BEFORE `users.sessions_valid_from`
    /// is refused — and it must change nothing else: a NULL timestamp (every account that never
    /// rotated), a session minted at or after the change, and a session with no `iat` on an
    /// unrestricted row all stay exactly as they were.
    #[test]
    fn a_session_minted_before_the_password_change_is_refused_and_nothing_else_is() {
        // NULL column: never restricted, so every token already in the wild keeps working.
        assert!(!session_predates_password_change(Some(1_000), None));
        // Minted BEFORE the change -> refused.
        assert!(session_predates_password_change(Some(1_000), Some(2_000)));
        // Minted AT the change or after it -> allowed (a re-login must work).
        assert!(!session_predates_password_change(Some(2_000), Some(2_000)));
        assert!(!session_predates_password_change(Some(3_000), Some(2_000)));
        // A session that cannot prove it is newer is refused rather than trusted.
        assert!(session_predates_password_change(None, Some(2_000)));
        assert!(!session_predates_password_change(None, None));
    }
}
