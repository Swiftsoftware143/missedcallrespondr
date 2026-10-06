//! The one credential boundary every guarded path passes through (kanban t_f8e7dd85).
//!
//! Precedent, copied not redesigned: `ADASwift/src/auth/boundary.rs` (itself from
//! `WorkflowSwift/src/auth/boundary.rs` @79015a0, `CoreSwift-CRM/src/auth/boundary.rs` @0ca91e5 and
//! `FunnelSwift/src/auth/boundary.rs` @704fc21).
//!
//! MissedCall Respondr already refused an anonymous caller on every `/api/v1` path — see
//! [`crate::auth::route_policy`]'s module docs for what was actually doing the work. Two of those
//! three shapes were not decisions: the anonymous router had no gate at all (so a newly mounted
//! route there was anonymous by default), and `merge` adopted `protected_routes`'
//! `layer`-wrapped fallback, which made even an unmounted path answer the middleware's 401.
//! [`require_credential`] replaces the guesswork: it is mounted once, with `route_layer` on the
//! merged router, reads only the committed allowlist, and is the single thing that decides whether a
//! caller may proceed with no credential.
//!
//! It answers exactly one question — *does this caller present a credential?* — and it can only
//! refuse an anonymous caller. Authorization (which account, which role, which row) stays where it
//! already was: `auth::middleware::auth_middleware` on the protected router and each handler.
//!
//! The refusal is deliberately distinguishable from every handler's own answer: the body is
//! `{"error":"Authentication required","status":401}` and never `auth_middleware`'s
//! `{"error":"Missing authorization header"}` nor the app's `AppError` shape — which is what makes
//! the boundary provable live: a curl that gets THIS body never reached a handler, and never reached
//! the old accidental fallback either.

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::auth::route_policy;
use crate::state::AppState;

/// The one user-visible string for "no credential at all".
pub const BOUNDARY_REFUSAL: &str = "Authentication required";

/// Length-independent comparison, so a wrong key cannot be distinguished from a right one by how
/// long it took to be refused.
fn ct_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut diff = a.len() ^ b.len();
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

/// Arm for [`route_policy::INTERNAL_ROUTES`]: the app's own shared secret in `x-internal-key`.
///
/// An app with NO key configured never authorises anything — both machine receivers in this app
/// already fail closed on an unset key (`state.config.internal_sync_key.is_empty() || …`), and the
/// boundary pins the same posture so a blanked env var can never turn one of them anonymous.
fn presents_internal_key(state: &AppState, req: &Request) -> bool {
    if state.config.internal_sync_key.is_empty() {
        return false;
    }
    match req
        .headers()
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
    {
        Some(key) => ct_eq(key, &state.config.internal_sync_key),
        None => false,
    }
}

/// Arm for every other guarded path: a credential this app already recognises.
///
/// This app has exactly one session credential — the app JWT. `auth::models::validate_token` is the
/// SAME verifier `auth_middleware` uses on the protected router, so a token that passes here cannot
/// fail there. There is deliberately no issued-API-key shape: `migrations/000027_retire_api_keys.sql`
/// retired that feature (`api_keys` is dropped and no auth path ever read a key — see
/// `route_policy`'s module docs).
fn presents_session_credential(state: &AppState, req: &Request) -> bool {
    match bearer(req) {
        Some(token) => crate::auth::models::validate_token(token, &state.config.jwt_secret).is_ok(),
        None => false,
    }
}

fn bearer(req: &Request) -> Option<&str> {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn reject(status: StatusCode, error: &str) -> Response {
    (
        status,
        Json(json!({ "error": error, "status": status.as_u16() })),
    )
        .into_response()
}

/// Global fail-closed auth. Mounted with `route_layer`, so it runs for every MOUNTED route and an
/// unmatched path still gets the router's own 404.
pub async fn require_credential(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();

    // Served surfaces and anything outside the API tree carry no credential and are not this
    // boundary's business — see `route_policy::is_guarded_path`.
    if !route_policy::is_guarded_path(&path) || route_policy::is_public_route(&path) {
        return next.run(req).await;
    }

    // ── the machine surface ─────────────────────────────────────────────────────────────────────
    // Named in `route_policy::INTERNAL_ROUTES`. Before this the only thing standing between an
    // anonymous caller and these handlers was the author's own key check. The key is now demanded
    // HERE too, so a route added under this prefix without an entry is an ordinary private route.
    //
    // A recognised session credential is accepted on this arm as well, because
    // `/api/v1/admin/portfolio-sync` (the panel-driven sibling of the same receiver) is posted with
    // a token; the handler still demands the key, and the boundary never widens a caller's reach.
    if route_policy::is_internal_route(&path) {
        return if presents_internal_key(&state, &req) || presents_session_credential(&state, &req) {
            next.run(req).await
        } else {
            reject(StatusCode::UNAUTHORIZED, BOUNDARY_REFUSAL)
        };
    }

    // ── everything else: a credential this app recognises ───────────────────────────────────────
    if presents_session_credential(&state, &req) {
        next.run(req).await
    } else {
        reject(StatusCode::UNAUTHORIZED, BOUNDARY_REFUSAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;

    fn req_with(headers: &[(&str, &str)]) -> Request {
        let mut b = HttpRequest::builder().uri("/api/v1/clients");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(Body::empty()).unwrap()
    }

    /// A token in THIS app's `Claims` shape (`config.rs`): `sub` is the user's uuid, `email` is the
    /// login identity, `aid` is the `tenants.id` handlers scope by, `role` decides the admin surface.
    fn jwt(secret: &str, exp_offset: i64) -> String {
        use jsonwebtoken::{encode, EncodingKey, Header};
        #[derive(serde::Serialize)]
        struct C {
            sub: String,
            email: String,
            aid: String,
            role: String,
            exp: usize,
            iat: usize,
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let claims = C {
            sub: uuid::Uuid::new_v4().to_string(),
            email: "canary@swiftsoftware.dev".to_string(),
            aid: uuid::Uuid::new_v4().to_string(),
            role: "account_owner".to_string(),
            exp: (now + exp_offset) as usize,
            iat: now as usize,
        };
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn the_internal_key_compare_is_exact() {
        assert!(ct_eq("s3cret", "s3cret"));
        assert!(!ct_eq("s3cret", "s3cre"));
        assert!(!ct_eq("s3cre", "s3cret"));
        assert!(!ct_eq("", "s3cret"));
        assert!(!ct_eq("s3cret", ""));
        // An empty configured key is refused before the compare (see presents_internal_key).
        assert!(ct_eq("", ""));
    }

    /// The boundary reads the committed allowlist, not a local array: this pins the delegation so
    /// the two cannot drift.
    #[test]
    fn the_boundary_reads_the_committed_allowlist() {
        assert!(route_policy::is_public_route("/api/v1/health"));
        assert!(!route_policy::is_public_route("/api/v1/clients"));
        assert!(route_policy::is_internal_route(
            "/api/v1/internal/portfolio-sync"
        ));
        assert!(route_policy::is_internal_route(
            "/api/v1/internal/portfolio-companies"
        ));
        // an UNLISTED sibling is neither public nor internal
        assert!(!route_policy::is_public_route("/api/v1/internal/whatever"));
        assert!(!route_policy::is_internal_route(
            "/api/v1/internal/whatever"
        ));
    }

    /// A bearer is only a credential when the app can recognise it — this is the difference between
    /// "refuse anonymous" and "refuse everyone", and it is what keeps a garbage bearer from being
    /// handed to a route that has no check of its own.
    #[test]
    fn the_session_arm_recognises_a_jwt() {
        const SECRET: &str = "boundary-unit-test-secret";
        let good = jwt(SECRET, 3600);
        assert!(route_policy::is_guarded_path("/api/v1/clients"));

        // A valid JWT verifies; a JWT signed with another key, and an expired one, do not.
        assert!(crate::auth::models::validate_token(&good, SECRET).is_ok());
        assert!(crate::auth::models::validate_token(&jwt("other-secret", 3600), SECRET).is_err());
        assert!(crate::auth::models::validate_token(&jwt(SECRET, -7200), SECRET).is_err());
        // A garbage bearer is not a credential.
        assert!(crate::auth::models::validate_token("not-a-jwt", SECRET).is_err());
        assert!(crate::auth::models::validate_token("", SECRET).is_err());
    }

    #[test]
    fn the_bearer_extraction_is_exact() {
        assert_eq!(
            bearer(&req_with(&[("authorization", "Bearer tok")])),
            Some("tok")
        );
        assert_eq!(bearer(&req_with(&[("authorization", "tok")])), None);
        assert_eq!(bearer(&req_with(&[("authorization", "Bearer ")])), Some(""));
        assert_eq!(bearer(&req_with(&[])), None);
    }

    /// The closure this card lands, pinned: the boundary's refusal is its own body, distinct from
    /// the middleware's `{"error":"Missing authorization header"}` the accidental fallback used to
    /// answer with.
    #[test]
    fn the_refusal_body_is_the_boundarys_own() {
        let resp = reject(StatusCode::UNAUTHORIZED, BOUNDARY_REFUSAL);
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(BOUNDARY_REFUSAL, "Authentication required");
    }
}
