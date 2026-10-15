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
    const ADMIN_SURFACE: &[&str] = &["/api/v1/admin", "/api/v1/payment-providers"];
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

    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::{
        admin_surface_denied, admin_surface_denied_for, is_impersonated_session, is_platform_admin,
        is_stop_impersonation,
    };

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
}
