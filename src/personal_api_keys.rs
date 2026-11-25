//! The fleet-standard mint surface `/api/personal-api-keys` (P1c, spec
//! `docs/p1c-personal-api-keys-spec-2026-11-25.md`).
//!
//! ONE implementation, two names. These five routes are wrappers over the existing
//! `handlers::api_key_handler` family — hashing, sealing, the 10-key cap, audit and tenancy all
//! stay in that one module, never re-implemented here (a second credential implementation is the
//! defect class the fleet has already paid for). Responses are the app's existing ones: list is
//! `{"api_keys":[…]}`, mint returns the plaintext under `token`, and the `:id` handlers answer the
//! same bodies as their `/api/v1/api-keys` twins.
//!
//! Mounted with `.nest("/api/personal-api-keys", …)` INSIDE `protected_routes` in `routes.rs`, so
//! the router's own `auth_middleware` `route_layer` covers every path here — the handlers take
//! `Extension<Claims>`, which only that middleware inserts. The path is deliberately OUTSIDE the
//! `/api/v1` tree, so `auth::boundary::require_credential` (which governs only `/api/v1/*`) passes
//! it through and `auth_middleware` is the single credential check, exactly like every route on
//! the protected router.
//!
//! The routes (relative to the nest prefix):
//!   GET    /                  → `list_api_keys`
//!   POST   /                  → `create_api_key` (mint)
//!   DELETE /:id               → `revoke_api_key`
//!   POST   /:id/rotate        → `regenerate_api_key` (atomic retire-then-mint, role preserved)
//!   POST   /:id/reveal        → `reveal_api_key` (session only, no password, audited)

use axum::{routing, Router};

use crate::handlers::api_key_handler;
use crate::state::AppState;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/",
            routing::get(api_key_handler::list_api_keys).post(api_key_handler::create_api_key),
        )
        .route("/:id", routing::delete(api_key_handler::revoke_api_key))
        .route(
            "/:id/rotate",
            routing::post(api_key_handler::regenerate_api_key),
        )
        .route(
            "/:id/reveal",
            routing::post(api_key_handler::reveal_api_key),
        )
        .with_state(state)
}
