//! Regression armour for card t_eb7736b8: the empty-`x-internal-key` bypass.
//!
//! A handler that compares `key != expected` without refusing an EMPTY `expected` answers a
//! request whose `x-internal-key` header is empty or absent on any host where
//! `INTERNAL_SYNC_KEY` is unset. `config.rs::required_secret` already refuses to boot with an
//! empty `INTERNAL_SYNC_KEY` (t_2a1306a7), so the live container cannot reach that branch —
//! these legs pin the *handler-side* guard so the class cannot come back if that config policy
//! is ever relaxed, or if a state is built some other way (this file builds one).
//!
//! Every leg drives the REAL handler against a state whose `internal_sync_key` is empty, and the
//! `*_old_shape_*` control legs evaluate the exact pre-fix expression on the same inputs. That
//! contrast is what makes the passing legs evidence instead of a tautology: it shows the old
//! expression really did authorise the request the new one refuses.
#![cfg(test)]

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue};
use serde_json::json;

use super::portfolio_handler;
use super::portfolio_sync_handler;
use super::swallow_tests::{capture, test_state};
use super::tag_provision_handler::{self, TagProvisionRequest};
use crate::error::AppError;

/// The swallow harness's state (dead pool, so nothing can be written) with the ONE difference
/// that matters here: an EMPTY configured internal key — the state a host with
/// `INTERNAL_SYNC_KEY` unset has.
fn empty_key_state() -> crate::state::AppState {
    let mut state = test_state();
    state.config.internal_sync_key = String::new();
    state
}

fn keyed_state(key: &str) -> crate::state::AppState {
    let mut state = test_state();
    state.config.internal_sync_key = key.to_string();
    state
}

/// `None` = header absent; `Some("")` = header present but empty; `Some(x)` = wrong key.
fn headers(presented: Option<&str>) -> HeaderMap {
    let mut map = HeaderMap::new();
    if let Some(value) = presented {
        map.insert(
            "x-internal-key",
            HeaderValue::from_str(value).expect("valid header value"),
        );
    }
    map
}

fn refused<T>(res: &Result<T, AppError>) -> bool {
    matches!(res, Err(AppError::Unauthorized(_)))
}

/// The three unauthenticated shapes a caller can present. The guard must refuse all of them
/// when the configured key is empty.
const UNAUTHENTICATED: [Option<&str>; 3] = [None, Some(""), Some("wrong-key")];

#[test]
fn portfolio_create_refuses_absent_and_empty_header_on_an_empty_configured_key() {
    for presented in UNAUTHENTICATED {
        let (res, _log) = capture(move || {
            let state = empty_key_state();
            let h = headers(presented);
            async move {
                portfolio_handler::internal_create_portfolio_company(
                    State(state),
                    h,
                    axum::Json(json!({"tenant_id": "883a2a82-c7e4-4abb-b6c2-da47c119caf1"})),
                )
                .await
            }
        });
        assert!(
            refused(&res),
            "portfolio-companies must refuse {presented:?} when the configured key is empty"
        );
    }
}

#[test]
fn portfolio_sync_refuses_absent_and_empty_header_on_an_empty_configured_key() {
    for presented in UNAUTHENTICATED {
        let (res, _log) = capture(move || {
            let state = empty_key_state();
            let h = headers(presented);
            async move {
                portfolio_sync_handler::portfolio_sync_internal(
                    State(state),
                    h,
                    axum::Json(json!({"action": "create"})),
                )
                .await
            }
        });
        assert!(
            refused(&res),
            "portfolio-sync must refuse {presented:?} when the configured key is empty"
        );
    }
}

#[test]
fn tag_provision_refuses_absent_and_empty_header_on_an_empty_configured_key_without_logging_it() {
    for presented in UNAUTHENTICATED {
        let (res, log) = capture(move || {
            let state = empty_key_state();
            let h = headers(presented);
            let body: TagProvisionRequest = serde_json::from_value(json!({
                "contact": {"email": "guard-probe@example.invalid"},
                "tag": {"name": "guard-probe"},
                "source": "test",
                "timestamp": "test"
            }))
            .expect("probe body deserializes");
            async move {
                tag_provision_handler::handle_tag_provision(State(state), h, axum::Json(body)).await
            }
        });
        assert!(
            refused(&res),
            "tag-provision must refuse {presented:?} when the configured key is empty"
        );
        assert!(
            log.contains("presented_len="),
            "the refusal must be logged by length, not by value: {log}"
        );
        assert!(
            !log.contains("wrong-key"),
            "an invalid key must never reach the log stream: {log}"
        );
    }
}

#[test]
fn old_shape_authorised_an_absent_header_on_an_empty_configured_key() {
    // Control: the exact pre-fix expression, on the same inputs the legs above use. `key` came
    // from `.unwrap_or("")`, so an absent header compared EQUAL to an empty configured key and
    // the gate fell through. This must stay true or the legs above prove nothing.
    let key = "";
    let expected = "";
    assert!(
        !(key != expected),
        "control: the pre-fix expression authorised an absent header against an empty key"
    );
}

#[test]
fn a_real_key_still_passes_the_gate_and_a_wrong_one_does_not() {
    // Both directions, on the same handler: a tightened gate that locked out the real caller
    // would be a different bug, so the positive leg matters as much as the negative one.
    let (res, _log) = capture(|| {
        let state = keyed_state("guard-test-key");
        async move {
            portfolio_sync_handler::portfolio_sync_internal(
                State(state),
                headers(Some("guard-test-key")),
                axum::Json(json!({"action": "create"})),
            )
            .await
        }
    });
    assert!(
        !refused(&res),
        "the configured key must still pass the gate: {res:?}"
    );

    let (res, _log) = capture(|| {
        let state = keyed_state("guard-test-key");
        async move {
            tag_provision_handler::handle_tag_provision(
                State(state),
                headers(Some("not-the-key")),
                axum::Json(
                    serde_json::from_value::<TagProvisionRequest>(json!({
                        "contact": {"email": "guard-probe@example.invalid"},
                        "tag": {"name": "guard-probe"},
                        "source": "test",
                        "timestamp": "test"
                    }))
                    .expect("probe body deserializes"),
                ),
            )
            .await
        }
    });
    assert!(
        refused(&res),
        "a wrong key must be refused, not answered by the handler"
    );
}
