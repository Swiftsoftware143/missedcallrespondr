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
    if admin_surface_denied(req.uri().path(), &claims.role) {
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
    use super::{admin_surface_denied, is_platform_admin};

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
        ] {
            assert!(
                !admin_surface_denied(path, "account_owner"),
                "{path} is not the admin surface"
            );
        }
    }
}
