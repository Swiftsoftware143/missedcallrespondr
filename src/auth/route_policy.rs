//! Default-deny routing for the mounted surface (kanban t_f8e7dd85). Precedent, copied not
//! redesigned: `ADASwift/src/auth/route_policy.rs` @HEAD, `WorkflowSwift/src/auth/route_policy.rs`
//! @79015a0, `CoreSwift-CRM/src/auth/route_policy.rs` @0ca91e5, `FunnelSwift/src/auth/route_policy.rs`
//! @704fc21, `IncentiveSwift/src/security/route_policy.rs` @47c909a4.
//!
//! # The rule
//!
//! **A mounted route is PRIVATE unless it appears in one of the two lists below.**
//! [`crate::auth::boundary::require_credential`] is mounted once, on the merged router, and reads
//! only this module. Nothing else decides whether a route may be reached anonymously.
//!
//! # The census (measured 2026-10-06 from `src/routes.rs`, then verified live with an anonymous
//! probe against 127.0.0.1:8088)
//!
//! ```text
//!   113 mounted `.route(..)` entries in the one routing file, 113 distinct paths (every path is
//!       mounted once; `:id`-style templates make each one unique)
//!
//!   100 entries reach `protected_routes`, which carries `auth_middleware`
//!    13 entries reach the ANONYMOUS `public_routes`:
//!        10 deliberate public routes  (this module's PUBLIC_ROUTES)
//!         3 machine receivers whose credential is the app's own `x-internal-key`
//!           (`/api/v1/internal/portfolio-companies`, `/api/v1/internal/portfolio-sync`,
//!            `/api/v1/internal/provision-free-account`)
//! ```
//!
//! # What this app's contribution is
//!
//! MissedCall Respondr already REFUSED an anonymous caller on every path — but two of the three
//! shapes doing that work were not decisions, and one of them was a defect:
//!
//! 1. **The anonymous router had no gate at all** (the default-allow shape). Public-versus-private
//!    was decided by WHICH sub-router an author registered a route in, with no list, no test and no
//!    boundary: a route mounted in `public_routes` tomorrow would be anonymous by default, and the
//!    only thing standing between a stranger and the two machine receivers was the author's own key
//!    check inside each handler. This module is the list; [`crate::auth::boundary::require_credential`]
//!    is the boundary. A route added to `public_routes` without an entry here answers 401 anonymous
//!    instead of answering its handler.
//! 2. **The whole router was default-deny by ACCIDENT.** `protected_routes` was mounted with
//!    `Router::layer(auth_middleware)` (and the body-read deadline below it); in axum 0.7 `layer`
//!    wraps the router's *fallback* as well, which flips the router's `default_fallback` flag, and
//!    `merge` then ADOPTS that wrapped fallback for the merged router. Measured live before this
//!    change: `GET /whatever` — a path nothing mounts — answered
//!    `401 {"error":"Missing authorization header"}`, i.e. the middleware's own body for a route that
//!    does not exist, and the documented answer (the router's own 404) was unreachable. Both layers
//!    now use `route_layer`, which applies to the routes in the router and NOT to its fallback, so
//!    unmatched paths answer the router's own 404 and the boundary — not a coincidence of layering —
//!    is what refuses an anonymous caller. The refusal is distinguishable: the boundary's body is
//!    `{"error":"Authentication required","status":401}` and never `auth_middleware`'s
//!    `{"error":"Missing authorization header"}`.
//! 3. **Two accidental-anonymous routes were looked for and NOT found.** All 99 routes on the gated
//!    router take a credential, and both anonymous machine receivers check the shared key themselves
//!    — each fails CLOSED (`internal_sync_key.is_empty() || key != internal_sync_key`), so an
//!    unconfigured key refuses instead of admitting. The 10 deliberate public routes are named with
//!    their reason below. This app's census is a TIGHTENING (a committed list + a boundary + the
//!    fallback closure), not a repair of anonymous handlers — the same honest finding the three
//!    apps before it reached.
//!
//! # Credentials accepted
//!
//! * **App JWT** — `Authorization: Bearer <token>`, HS256 over `JWT_SECRET`, verified with the SAME
//!   [`crate::auth::models::validate_token`] `auth_middleware` uses, so a token that passes here
//!   cannot fail there. This app's `Claims` carry `sub` (the user id), `email`, `aid` (the
//!   `tenants.id` every handler scopes by) and `role`.
//! * **`x-internal-key`** — the app's own `INTERNAL_SYNC_KEY`, for [`INTERNAL_ROUTES`] only, never
//!   accepted when the app has no key configured.
//!
//! There is deliberately NO issued-API-key arm here: this app RETIRED the feature.
//! `migrations/000027_retire_api_keys.sql` drops the table, the `POST /api/v1/api-keys` group is
//! gone, and `src/routes.rs` records that no auth path ever read a key (`grep`: no `x-api-key`
//! anywhere). Naming a key shape the app cannot resolve would have been the opposite of a boundary.
//!
//! The boundary never *widens* a caller's reach: it decides nothing about tenancy, roles, or which
//! account a request may touch. It can only refuse a caller that presents no credential at all.
//!
//! # Adding a route
//!
//! Leave it out of both lists and it is private. Add an entry only when the route must answer a
//! caller that presents no credential — and add the shape to the test module below, so the decision
//! and its reason are recorded with the code.

/// Routes that may be reached with NO credential at all.
///
/// Templates use the app's own axum-0.7 `:param` spelling and match by segment (see
/// [`matches_template`]), so `/api/v1/checkout/session/:id` accepts one segment there and never a
/// longer path.
pub const PUBLIC_ROUTES: &[&str] = &[
    // --- liveness ------------------------------------------------------------------------------
    // Read by nginx, by the fleet uptime watchdog and by this app's deploy engine, which fails the
    // deploy on anything but 200. Returns the service name and crate version only.
    "/api/v1/health",
    // --- account entry points ------------------------------------------------------------------
    // A credential is CREATED at these routes, so they are anonymous by definition: self-service
    // registration (it mints the `tenants` row and the `account_owner` user), login, and the
    // password-recovery pair.
    "/api/v1/auth/register",
    "/api/v1/auth/login",
    "/api/v1/auth/forgot-password",
    "/api/v1/auth/reset-password",
    // --- public catalogue ----------------------------------------------------------------------
    // Read BEFORE a session exists: the signup wizard and the integrations screen populate their
    // provider picker from it. Returns the platform integration vocabulary (key, name, icon) and is
    // scoped by no account.
    "/api/v1/available-providers",
    // --- telephony receiver whose own SIGNATURE is the credential ------------------------------
    // Telnyx posts call-control events here with no session. The receiver is the app's inbound
    // telephony door; it is mounted on the anonymous router because Telnyx cannot present a session,
    // and it is also mounted with the body-read deadline. This is a receiver surface, not a data
    // surface. (Its own verification gap — a delivery that arrives unsigned is not refused here —
    // is NAMED on the card as a separate job, not hidden by this list.)
    "/api/v1/telnyx/webhook",
    // --- payment receivers whose own SIGNATURE is the credential --------------------------------
    // Stripe and PayPal post here with no session; each receiver verifies the HMAC over the raw body
    // (and, for Stripe, the freshness of the `t=` stamp) and refuses a delivery it cannot verify.
    // Both read the raw bytes before they check anything, which is why both are also mounted with
    // the body-read deadline.
    "/api/v1/webhooks/stripe",
    "/api/v1/webhooks/paypal",
    // --- public checkout-session polling -------------------------------------------------------
    // `get_checkout_session_public` reads one session by its UUID (unguessable) and returns the
    // status summary the post-checkout page shows. Read-only, and no tenant scope is decided here.
    "/api/v1/checkout/session/:id",
];

/// Service-to-service routes whose own shared key (`x-internal-key` = `INTERNAL_SYNC_KEY`) is the
/// credential.
///
/// All three handlers already check that key themselves — the boundary demands it as well, so that a
/// route added under one of these prefixes without an entry here is an ordinary PRIVATE route rather
/// than one whose safety depends on its author remembering. Nothing here is anonymous: the key is
/// the caller's credential, and the boundary refuses a caller that presents none.
pub const INTERNAL_ROUTES: &[&str] = &[
    // The three machine receivers, all POST-only, all mounted on the anonymous router because a
    // sibling app presents a key rather than a session. They deserialize their `Json` body BEFORE
    // the handler body runs, i.e. an anonymous POST reaches handler code today — which is exactly
    // why the key is demanded at the boundary as well as inside the handler.
    //
    // `/api/v1/internal/portfolio-companies` receives the portfolio-company push;
    // `/api/v1/internal/portfolio-sync` receives the whole-tenant portfolio sync;
    // `/api/v1/internal/provision-free-account` receives FunnelSwift's tag → free-account request
    // (kanban t_1d08bd9a, design §3.1) — the receiver that turns `MissedCall Respondr — Free` on a
    // lead into a real account this business can log into and upgrade in place.
    //
    // None is registered under the `/api/v1/admin/*` prefix: that prefix carries the platform-admin
    // ROLE gate, and a machine door must not be closed by a tenant-role decision.
    "/api/v1/internal/portfolio-companies",
    "/api/v1/internal/portfolio-sync",
    "/api/v1/internal/provision-free-account",
];

/// Is this path inside the API surface this boundary decides?
///
/// Only the `/api/v1` tree. The marketing page, the legal pages and the other static files nginx
/// serves from `/opt/swift/nginx/www/missedcall/**` never reach a router here and carry no
/// credential — they are outside the boundary by construction, and a path that is not guarded is
/// passed through untouched.
pub fn is_guarded_path(path: &str) -> bool {
    path == "/api/v1" || path.starts_with("/api/v1/")
}

/// May this path be reached with NO credential at all?
pub fn is_public_route(path: &str) -> bool {
    PUBLIC_ROUTES.iter().any(|t| matches_template(t, path))
}

/// Is this a service-to-service route, reached with the app's own shared key?
pub fn is_internal_route(path: &str) -> bool {
    INTERNAL_ROUTES.iter().any(|t| matches_template(t, path))
}

/// Does one template match one concrete path?
///
/// Segment-wise: the split lengths must agree and every template segment is either a param (`:id`,
/// this crate's axum 0.7 spelling, or `{id}`) or the identical literal. Deliberately stricter than a
/// string prefix — `/api/v1/telnyxX` is a different route and must not be caught, and a template can
/// never accidentally swallow a longer path such as `/api/v1/checkout/session/x/extra`.
fn matches_template(template: &str, path: &str) -> bool {
    let t: Vec<&str> = template.split('/').collect();
    let p: Vec<&str> = path.split('/').collect();
    if t.len() != p.len() {
        return false;
    }
    t.iter().zip(p.iter()).all(|(tseg, pseg)| {
        let is_param = tseg.starts_with(':')
            || (tseg.starts_with('{') && tseg.ends_with('}') && tseg.len() > 2);
        if is_param {
            !pseg.is_empty()
        } else {
            tseg == pseg
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{is_guarded_path, is_internal_route, is_public_route, matches_template};

    /// Every `.route(..)` this app mounts lives in this one file, so the census is a scan of it.
    const ROUTES_SRC: &str = include_str!("../routes.rs");

    /// Every `.route("<literal>"` path literal in the routing file, in source order.
    fn route_literals() -> Vec<&'static str> {
        let bytes = ROUTES_SRC.as_bytes();
        let mut out = Vec::new();
        let mut i = 0usize;
        while let Some(pos) = ROUTES_SRC[i..].find(".route(") {
            let mut j = i + pos + ".route(".len();
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                let start = j + 1;
                let mut k = start;
                while k < bytes.len() && bytes[k] != b'"' {
                    k += 1;
                }
                out.push(&ROUTES_SRC[start..k]);
            }
            i = j;
        }
        out
    }

    /// Is this keyed path backed by a route the router really mounts?
    ///
    /// The allowlist is written with the `/api/v1` prefix the boundary sees; the literals in
    /// `routes.rs` are the same absolute paths, so an entry is backed when some mounted literal is
    /// equal to it, or when a mounted PARAMETERISED literal matches it segment-wise (the allowlist
    /// and the mounts both use this crate's `:param` spelling).
    fn mounted(entry: &str) -> Option<&'static str> {
        route_literals()
            .into_iter()
            .find(|lit| !lit.is_empty() && (*lit == entry || matches_template(lit, entry)))
    }

    /// Every allowlist entry must name a route the router really mounts. A stale entry is a live
    /// auth widening waiting for a new route to be mounted at that path.
    #[test]
    fn every_allowlist_entry_names_a_mounted_route() {
        for entry in super::PUBLIC_ROUTES
            .iter()
            .chain(super::INTERNAL_ROUTES.iter())
        {
            assert!(
                mounted(entry).is_some(),
                "allowlist entry {} names a route src/routes.rs does not mount",
                entry
            );
            assert!(
                entry.starts_with("/api/v1/"),
                "{} is outside the guarded prefix",
                entry
            );
        }
        // the scan really sees the mounts (a silent zero would make the test vacuous)
        assert!(
            route_literals().len() >= 113,
            "route literal scan found too few: {}",
            route_literals().len()
        );
    }

    #[test]
    fn neither_list_has_duplicates() {
        for list in [super::PUBLIC_ROUTES, super::INTERNAL_ROUTES] {
            let mut seen = std::collections::HashSet::new();
            for entry in list {
                assert!(seen.insert(*entry), "duplicate allowlist entry {}", entry);
            }
        }
    }

    #[test]
    fn the_two_lists_are_disjoint() {
        for entry in super::PUBLIC_ROUTES {
            assert!(
                !super::INTERNAL_ROUTES.contains(entry),
                "{} is in both lists — an anonymous entry wins by accident",
                entry
            );
        }
    }

    /// Tenant and operator surfaces must never be anonymous.
    #[test]
    fn tenant_and_operator_surfaces_are_not_public() {
        for path in [
            "/api/v1/auth/me",
            "/api/v1/me/usage",
            "/api/v1/auth/profile",
            "/api/v1/auth/password",
            "/api/v1/calls",
            "/api/v1/calls/call-one",
            "/api/v1/contacts",
            "/api/v1/contacts/contact-one",
            "/api/v1/leads",
            "/api/v1/deals",
            "/api/v1/tickets",
            "/api/v1/tickets/ticket-one",
            "/api/v1/campaigns",
            "/api/v1/messages",
            "/api/v1/lists",
            "/api/v1/tags",
            "/api/v1/dashboard/stats",
            "/api/v1/dashboard/activity",
            "/api/v1/portfolio-companies",
            "/api/v1/integration-targets",
            "/api/v1/provider-keys",
            "/api/v1/telnyx/numbers",
            "/api/v1/clients",
            "/api/v1/clients/client-one",
            "/api/v1/provider-keys/stripe/test",
            "/api/v1/integrations/coreswift/status",
            "/api/v1/integrations/coreswift/push",
            "/api/v1/admin/plans",
            "/api/v1/admin/tenants",
            "/api/v1/admin/site",
            "/api/v1/admin/email-config",
            "/api/v1/admin/impersonate",
            "/api/v1/admin/portfolio-sync",
            "/api/v1/payment-providers",
            "/api/v1/checkout/create",
            "/api/v1/checkout/sessions",
        ] {
            assert!(!is_public_route(path), "{} must not be anonymous", path);
            assert!(!is_internal_route(path), "{} must not be a key route", path);
        }
    }

    /// ...while the deliberate participant surfaces stay public, so a future lane cannot "harden"
    /// the app by deleting a signup or a payment webhook.
    #[test]
    fn the_deliberate_public_surfaces_stay_public() {
        for path in [
            "/api/v1/health",
            "/api/v1/auth/register",
            "/api/v1/auth/login",
            "/api/v1/auth/forgot-password",
            "/api/v1/auth/reset-password",
            "/api/v1/available-providers",
            "/api/v1/telnyx/webhook",
            "/api/v1/webhooks/stripe",
            "/api/v1/webhooks/paypal",
        ] {
            assert!(is_public_route(path), "{} must stay anonymous", path);
        }
        // A parameterised public entry matches exactly one segment there...
        assert!(is_public_route("/api/v1/checkout/session/session-one"));
        // ...and never a longer path, and never its plural sibling.
        assert!(!is_public_route("/api/v1/checkout/session/x/extra"));
        assert!(!is_public_route("/api/v1/checkout/sessions"));
    }

    #[test]
    fn internal_routes_are_named_and_not_public() {
        for path in [
            "/api/v1/internal/portfolio-companies",
            "/api/v1/internal/portfolio-sync",
            "/api/v1/internal/provision-free-account",
        ] {
            assert!(is_internal_route(path), "{} is a named key route", path);
            assert!(!is_public_route(path), "{} must not be anonymous", path);
        }
        // No `starts_with` arm exists to inherit: an UNLISTED sibling under the wired prefix is an
        // ordinary private route, decided by the boundary alone.
        for path in [
            "/api/v1/internal/whatever",
            "/api/v1/internal/portfolio-sync/extra",
            "/api/v1/internal/tag-provision",
            "/api/v1/internal/provision-free-account/extra",
        ] {
            assert!(!is_internal_route(path), "{} must be private", path);
            assert!(!is_public_route(path), "{} must be private", path);
        }
    }

    #[test]
    fn matching_is_segment_exact() {
        assert!(matches_template("/api/v1/clients", "/api/v1/clients"));
        assert!(!matches_template(
            "/api/v1/clients",
            "/api/v1/clients/extra"
        ));
        assert!(!matches_template("/api/v1/clients", "/api/v1/clientsX"));
        assert!(matches_template(
            "/api/v1/clients/:id",
            "/api/v1/clients/abc"
        ));
        assert!(matches_template(
            "/api/v1/clients/{id}",
            "/api/v1/clients/abc"
        ));
        assert!(!matches_template(
            "/api/v1/clients/:id",
            "/api/v1/clients//extra"
        ));
        assert!(!matches_template("/api/v1/clients/:id", "/api/v1/clients"));
        assert!(!matches_template(
            "/api/v1/clients/:id",
            "/api/v1/clients/a/b"
        ));
    }

    /// The served/non-API surface boundary: nothing outside `/api/v1` is decided here.
    #[test]
    fn served_surfaces_are_outside_the_guarded_prefix() {
        for path in [
            "/",
            "/robots.txt",
            "/sitemap.xml",
            "/index.html",
            "/terms.html",
            "/privacy.html",
            "/accessibility.html",
            "/guide.html",
            "/forgot-password.html",
        ] {
            assert!(!is_guarded_path(path), "{} must not be guarded", path);
        }
        assert!(is_guarded_path("/api/v1"));
        assert!(is_guarded_path("/api/v1/health"));
        assert!(!is_guarded_path("/api/v1x"));
        assert!(!is_guarded_path("/api/v2/health"));
        assert!(!is_guarded_path("/apix"));
    }

    /// The census shape the module docs claim, pinned so the numbers in the doc cannot drift away
    /// from the code without a test failing.
    #[test]
    fn the_census_shape_is_what_the_docs_say() {
        assert_eq!(super::PUBLIC_ROUTES.len(), 10, "PUBLIC_ROUTES size");
        assert_eq!(super::INTERNAL_ROUTES.len(), 3, "INTERNAL_ROUTES size");
        // The 13 anonymous mounts the census found: 10 deliberate + 3 key receivers.
        assert_eq!(
            super::PUBLIC_ROUTES.len() + super::INTERNAL_ROUTES.len(),
            13
        );
        // 113 entries / 113 distinct paths, and the two lists are disjoint.
        let lits = route_literals();
        assert_eq!(lits.len(), 113, "mounted .route entries");
        let mut uniq: Vec<&str> = lits.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), 113, "distinct mounted paths");
        for entry in super::PUBLIC_ROUTES {
            assert!(
                !super::INTERNAL_ROUTES.contains(entry),
                "{} in both lists",
                entry
            );
        }
    }
}
