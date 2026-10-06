//! Telnyx inbound-webhook signature verification (kanban t_0e4ae7b7).
//!
//! # Why this module exists
//!
//! `POST /api/v1/telnyx/webhook` (`handlers::telnyx_handler::webhook`) was mounted on the anonymous
//! router and verified NO credential of any kind: it took `Json<Value>`, read
//! `/data/event_type`, and dispatched on it. `TelnyxConfig.webhook_secret` existed and nothing on
//! earth read it (it was only reported as `has_webhook_secret` by the admin config read). The whole
//! delivery-status arm was therefore reachable by any stranger: a forged `message.delivered` /
//! `message.failed` moved a REAL outbound `messages` row (`queued` → `sent` → `delivered`, or
//! `failed`) by the provider's own message id, and a forged `call_received` drove the inbound-call
//! arm (credit deduction, `inbound_calls` / `call_logs` writes, the CoreSwift lead push and the
//! response-rule evaluation). This is the code CoreSwift-CRM's Telnyx module was ported FROM, so
//! the gap was shared rather than convergent — that card's twin is t_fd5000e1, and this is the
//! same contract landed here.
//!
//! # The scheme (Telnyx's documented one, as its own SDKs implement it)
//!
//! Mirrors `team-telnyx/telnyx-go/lib/webhook_verification.go` step for step:
//!
//! * `telnyx-signature-ed25519` — base64 of the 64-byte Ed25519 signature.
//! * `telnyx-timestamp` — unix seconds.
//! * the signed message is the raw byte string `{timestamp}|{request body}`.
//! * the public key is the ACCOUNT key pair's public half (base64, 32 bytes) from Telnyx Mission
//!   Control, supplied to this deployment as `TELNYX_PUBLIC_KEY`.
//! * a delivery whose timestamp is further than 5 minutes from now (Telnyx's own default) is
//!   refused, so a captured request cannot be replayed later. A still-fresh duplicate of an
//!   already-processed delivery is refused too — see [`ReplayGuard`].
//!
//! Verification MUST see the exact bytes the sender signed, which is why the receiver takes `Bytes`
//! and not `Json<Value>`: re-serialising a parsed `Value` reorders keys and drops whitespace, and
//! every genuine delivery would then fail.
//!
//! # The order of the arms, and why
//!
//! CONFIG → PRESENCE → AUTHENTICITY → FRESHNESS → REPLAY. Freshness may only judge a delivery that
//! is already authentic, so an unreadable or wrong-signature request is always reported as the
//! signature failure it is — never as a clock complaint (which would send an operator hunting a
//! host time bug that does not exist). The same rule the Stripe arm of this app follows
//! (t_4754e612).
//!
//! Refusal statuses are split by WHOSE fault the refusal is, because the receiver must stay
//! reachable anonymously (it cannot present a JWT) and Telnyx retries a non-2xx delivery:
//!
//! * `503 telnyx_verification_not_configured` / `503 telnyx_public_key_invalid` — our deployment's
//!   gap, not the sender's. Telnyx retries, and the retry succeeds once the key is set. Failing
//!   CLOSED is the whole point: an unverifiable delivery is never applied.
//! * `401 …` — the delivery itself failed (missing/stale/mismatched signature). A genuine Telnyx
//!   delivery re-signs with a fresh timestamp on every retry, so it recovers from a 401 too.

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine as _;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Header carrying the base64 Ed25519 signature (Telnyx's own name; case-insensitive lookup).
pub const SIGNATURE_HEADER: &str = "telnyx-signature-ed25519";

/// Header carrying the unix-second timestamp the signature covers.
pub const TIMESTAMP_HEADER: &str = "telnyx-timestamp";

/// The replay window in seconds when the deployment does not set one. Telnyx's own SDKs use 5
/// minutes; `TELNYX_SIGNATURE_TOLERANCE_SECS` overrides it (clamped in `config.rs`).
pub const DEFAULT_SIGNATURE_TOLERANCE_SECS: i64 = 300;

/// Length of a raw Ed25519 public key, in bytes.
const ED25519_PUBLIC_KEY_LEN: usize = 32;

/// Length of an Ed25519 signature, in bytes.
const ED25519_SIGNATURE_LEN: usize = 64;

/// A delivery that passed every arm, and the signature that lets a duplicate be recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDelivery {
    /// The delivered signature, base64 as it arrived. The replay key.
    pub signature: String,
    /// The delivered timestamp, in unix seconds.
    pub timestamp: i64,
}

/// A refused delivery: the status and the reason the sender is answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelnyxRejection {
    pub status: StatusCode,
    pub reason: &'static str,
    pub detail: String,
}

impl TelnyxRejection {
    fn new(status: StatusCode, reason: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            reason,
            detail: detail.into(),
        }
    }

    /// True when the refusal is OUR deployment's fault (a missing or malformed configured key), so
    /// the caller logs it as an error: nothing on this route can succeed until the key is fixed.
    pub fn is_configuration(&self) -> bool {
        self.status == StatusCode::SERVICE_UNAVAILABLE
    }
}

impl IntoResponse for TelnyxRejection {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "error": self.reason,
                "message": self.detail,
                "status": self.status.as_u16(),
            })),
        )
            .into_response()
    }
}

/// Current time in unix seconds — one clock read per request, passed to every arm so the freshness
/// verdict and the replay window cannot disagree about "now".
pub fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string())
}

/// Verify one Telnyx delivery against Telnyx's Ed25519 contract.
///
/// `public_key_b64` is this deployment's configured key (`None`/empty means the deployment has not
/// been configured, which is refused rather than waved through).
pub fn verify(
    public_key_b64: Option<&str>,
    headers: &HeaderMap,
    body: &[u8],
    now_unix: i64,
    tolerance_secs: i64,
) -> Result<VerifiedDelivery, TelnyxRejection> {
    // 1. CONFIG — fail closed. A delivery we cannot verify is never applied, even when that means
    //    refusing a genuine one until the operator pastes the key in.
    let Some(key) = public_key_b64.map(str::trim).filter(|k| !k.is_empty()) else {
        return Err(TelnyxRejection::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "telnyx_verification_not_configured",
            "TELNYX_PUBLIC_KEY is not set on this deployment, so no Telnyx delivery can be verified \
             and every one is refused. Set the account's Ed25519 public key from Telnyx Mission \
             Control.",
        ));
    };

    // 2. PRESENCE — both headers are part of the contract; one without the other verifies nothing.
    let Some(signature) = header(headers, SIGNATURE_HEADER).filter(|s| !s.trim().is_empty()) else {
        return Err(TelnyxRejection::new(
            StatusCode::UNAUTHORIZED,
            "telnyx_signature_missing",
            format!("Missing required header: {SIGNATURE_HEADER}"),
        ));
    };
    let Some(timestamp_raw) = header(headers, TIMESTAMP_HEADER).filter(|s| !s.trim().is_empty())
    else {
        return Err(TelnyxRejection::new(
            StatusCode::UNAUTHORIZED,
            "telnyx_signature_missing",
            format!("Missing required header: {TIMESTAMP_HEADER}"),
        ));
    };

    // 3. AUTHENTICITY — the key is ours, so a key we cannot parse is a configuration error; the
    //    signature is the sender's, so a signature that does not verify is a 401.
    let engine = base64::engine::general_purpose::STANDARD;
    let key_bytes = engine
        .decode(key.as_bytes())
        .ok()
        .filter(|b| b.len() == ED25519_PUBLIC_KEY_LEN);
    let Some(key_bytes) = key_bytes else {
        return Err(TelnyxRejection::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "telnyx_public_key_invalid",
            format!(
                "TELNYX_PUBLIC_KEY is not a base64 {ED25519_PUBLIC_KEY_LEN}-byte Ed25519 public key \
                 (decoded length {}); no delivery can be verified until it is replaced with the value \
                 from Mission Control.",
                engine.decode(key.as_bytes()).map(|b| b.len()).unwrap_or(0)
            ),
        ));
    };
    let signature_bytes = engine
        .decode(signature.trim().as_bytes())
        .ok()
        .filter(|b| b.len() == ED25519_SIGNATURE_LEN);
    let Some(signature_bytes) = signature_bytes else {
        return Err(TelnyxRejection::new(
            StatusCode::UNAUTHORIZED,
            "telnyx_signature_verification_failed",
            format!(
                "{SIGNATURE_HEADER} is not a base64 {ED25519_SIGNATURE_LEN}-byte Ed25519 signature"
            ),
        ));
    };

    // The signed bytes are built from the RAW timestamp header and the RAW body: no re-serialising,
    // no trimming of the body, nothing between what Telnyx signed and what is verified here.
    let mut signed = Vec::with_capacity(timestamp_raw.len() + 1 + body.len());
    signed.extend_from_slice(timestamp_raw.as_bytes());
    signed.push(b'|');
    signed.extend_from_slice(body);

    if ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key_bytes.as_slice())
        .verify(&signed, &signature_bytes)
        .is_err()
    {
        return Err(TelnyxRejection::new(
            StatusCode::UNAUTHORIZED,
            "telnyx_signature_verification_failed",
            "The Ed25519 signature does not match `telnyx-timestamp|body` for this key".to_string(),
        ));
    }

    // 4. FRESHNESS — only an AUTHENTIC delivery is judged by the clock.
    let Ok(timestamp) = timestamp_raw.trim().parse::<i64>() else {
        return Err(TelnyxRejection::new(
            StatusCode::UNAUTHORIZED,
            "telnyx_timestamp_invalid",
            format!("{TIMESTAMP_HEADER} is not a unix timestamp in seconds: {timestamp_raw:?}"),
        ));
    };
    let age = now_unix.saturating_sub(timestamp);
    if age.abs() > tolerance_secs {
        return Err(TelnyxRejection::new(
            StatusCode::UNAUTHORIZED,
            "telnyx_timestamp_out_of_tolerance",
            format!(
                "Delivery is {age}s from this server's clock (tolerance {tolerance_secs}s) — a \
                 captured delivery cannot be replayed beyond it"
            ),
        ));
    }

    Ok(VerifiedDelivery {
        signature,
        timestamp,
    })
}

/// Remembers the signatures of deliveries whose effect already landed, so the same signed request
/// cannot be applied twice inside the freshness window.
///
/// Why this is not "just" the timestamp window: a captured delivery is a valid bearer credential for
/// [`DEFAULT_SIGNATURE_TOLERANCE_SECS`], and replaying it inside that window would otherwise apply
/// the effect a second time (a second delivery transition, or — on the call arm — a second credit
/// deduction and a second pair of call rows). Marking happens AFTER the effect lands: a delivery
/// whose first attempt failed on the database must still be allowed to retry.
///
/// Scope: this is per-process state, which matches how this app is deployed (one container, one
/// binary). It bounds replay to the window; it is not a durable dedupe ledger — Telnyx's own retry
/// of a delivery we already processed is refused here, which is the idempotency we want.
#[derive(Debug, Default)]
pub struct ReplayGuard {
    seen: Mutex<HashMap<String, i64>>,
}

impl ReplayGuard {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, i64>> {
        // A poisoned lock still holds valid data for this purpose; never panic the request path.
        self.seen.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Forget signatures older than the window, then report whether this one was already processed.
    fn prune(seen: &mut HashMap<String, i64>, now_unix: i64, tolerance_secs: i64) {
        let window = tolerance_secs.max(0);
        seen.retain(|_, at| now_unix.saturating_sub(*at) <= window);
    }

    /// True when this exact signed delivery has already been processed inside the window.
    pub fn is_replayed(&self, signature: &str, now_unix: i64, tolerance_secs: i64) -> bool {
        let mut seen = self.lock();
        Self::prune(&mut seen, now_unix, tolerance_secs);
        seen.contains_key(signature)
    }

    /// Record a delivery whose effect has landed.
    pub fn ack(&self, signature: &str, now_unix: i64, tolerance_secs: i64) {
        let mut seen = self.lock();
        Self::prune(&mut seen, now_unix, tolerance_secs);
        seen.insert(signature.to_string(), now_unix);
    }

    /// How many deliveries are currently remembered — for tests and diagnostics. Test-only: the
    /// shipped binary has no reader for it, and a permanently-unused public method is dead code.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The process-wide replay guard the receiver uses: one signed delivery is one delivery.
pub fn replay_guard() -> &'static ReplayGuard {
    static GUARD: OnceLock<ReplayGuard> = OnceLock::new();
    GUARD.get_or_init(ReplayGuard::default)
}

/// The refusal for a delivery whose effect has already landed: the signature is authentic and
/// inside the window, but this exact signed request was already applied, so applying it again would
/// double the effect.
pub fn replayed_rejection() -> TelnyxRejection {
    TelnyxRejection::new(
        StatusCode::UNAUTHORIZED,
        "telnyx_signature_replayed",
        "This exact signed delivery was already processed; a replay applies no second effect"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    struct Key {
        public_b64: String,
        pair: Ed25519KeyPair,
    }

    fn key() -> Key {
        let rng = SystemRandom::new();
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("generate key");
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("parse key");
        Key {
            public_b64: base64::engine::general_purpose::STANDARD
                .encode(pair.public_key().as_ref()),
            pair,
        }
    }

    fn headers(signature: &str, timestamp: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            SIGNATURE_HEADER,
            HeaderValue::from_str(signature).expect("sig"),
        );
        h.insert(
            TIMESTAMP_HEADER,
            HeaderValue::from_str(timestamp).expect("ts"),
        );
        h
    }

    fn sign(key: &Key, timestamp: &str, body: &[u8]) -> String {
        let mut signed = Vec::new();
        signed.extend_from_slice(timestamp.as_bytes());
        signed.push(b'|');
        signed.extend_from_slice(body);
        base64::engine::general_purpose::STANDARD.encode(key.pair.sign(&signed).as_ref())
    }

    const NOW: i64 = 1_800_000_000;
    const BODY: &[u8] = br#"{"data":{"event_type":"message.delivered","payload":{"id":"msg_1"}}}"#;

    #[test]
    fn a_correctly_signed_delivery_verifies() {
        let k = key();
        let ts = NOW.to_string();
        let sig = sign(&k, &ts, BODY);
        let verified = verify(Some(&k.public_b64), &headers(&sig, &ts), BODY, NOW, 300)
            .expect("a genuine delivery must verify");
        assert_eq!(verified.signature, sig);
        assert_eq!(verified.timestamp, NOW);
    }

    #[test]
    fn an_unconfigured_deployment_refuses_rather_than_accepts() {
        for configured in [None, Some(""), Some("   ")] {
            let k = key();
            let ts = NOW.to_string();
            let sig = sign(&k, &ts, BODY);
            let rejection = verify(configured, &headers(&sig, &ts), BODY, NOW, 300)
                .expect_err("without a configured key nothing may be accepted");
            assert_eq!(rejection.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(rejection.reason, "telnyx_verification_not_configured");
            assert!(rejection.is_configuration());
        }
    }

    #[test]
    fn a_missing_header_is_refused() {
        let k = key();
        let ts = NOW.to_string();
        let sig = sign(&k, &ts, BODY);
        let mut h = headers(&sig, &ts);
        h.remove(SIGNATURE_HEADER);
        let rejection =
            verify(Some(&k.public_b64), &h, BODY, NOW, 300).expect_err("no signature header");
        assert_eq!(rejection.status, StatusCode::UNAUTHORIZED);
        assert_eq!(rejection.reason, "telnyx_signature_missing");

        let mut h2 = headers(&sig, &ts);
        h2.remove(TIMESTAMP_HEADER);
        let rejection =
            verify(Some(&k.public_b64), &h2, BODY, NOW, 300).expect_err("no timestamp header");
        assert_eq!(rejection.reason, "telnyx_signature_missing");
    }

    #[test]
    fn a_forged_signature_is_refused() {
        let k = key();
        let other = key();
        let ts = NOW.to_string();
        let forged = sign(&other, &ts, BODY);
        let rejection = verify(Some(&k.public_b64), &headers(&forged, &ts), BODY, NOW, 300)
            .expect_err("another key's signature must not pass");
        assert_eq!(rejection.status, StatusCode::UNAUTHORIZED);
        assert_eq!(rejection.reason, "telnyx_signature_verification_failed");

        // A signature that is valid base64 but not 64 bytes is the same verdict.
        let rejection = verify(Some(&k.public_b64), &headers("AAAA", &ts), BODY, NOW, 300)
            .expect_err("a short signature must not pass");
        assert_eq!(rejection.reason, "telnyx_signature_verification_failed");
    }

    #[test]
    fn a_tampered_body_is_refused() {
        let k = key();
        let ts = NOW.to_string();
        let sig = sign(&k, &ts, BODY);
        let tampered = br#"{"data":{"event_type":"message.delivered","payload":{"id":"msg_2"}}}"#;
        let rejection = verify(Some(&k.public_b64), &headers(&sig, &ts), tampered, NOW, 300)
            .expect_err("the signature covers the body, so a changed byte must fail");
        assert_eq!(rejection.reason, "telnyx_signature_verification_failed");
    }

    #[test]
    fn a_stale_or_future_delivery_is_refused_by_the_clock() {
        let k = key();
        // Authentically signed, but an hour old / an hour in the future.
        for ts_value in [NOW - 3600, NOW + 3600] {
            let ts = ts_value.to_string();
            let sig = sign(&k, &ts, BODY);
            let rejection = verify(Some(&k.public_b64), &headers(&sig, &ts), BODY, NOW, 300)
                .expect_err("outside the replay window");
            assert_eq!(rejection.status, StatusCode::UNAUTHORIZED);
            assert_eq!(rejection.reason, "telnyx_timestamp_out_of_tolerance");
        }
        // The boundary itself is inside the window: |now - ts| == tolerance verifies.
        let ts = (NOW - 300).to_string();
        let sig = sign(&k, &ts, BODY);
        verify(Some(&k.public_b64), &headers(&sig, &ts), BODY, NOW, 300)
            .expect("boundary is inside");
    }

    #[test]
    fn an_unreadable_timestamp_after_an_authentic_signature_is_refused() {
        let k = key();
        let ts = "not-a-clock";
        let sig = sign(&k, ts, BODY);
        let rejection = verify(Some(&k.public_b64), &headers(&sig, ts), BODY, NOW, 300)
            .expect_err("an unreadable stamp bounds nothing");
        assert_eq!(rejection.reason, "telnyx_timestamp_invalid");
    }

    #[test]
    fn a_clock_complaint_never_masks_a_signature_failure() {
        // Authenticity is judged before freshness: an ancient stamp on a signature that does NOT
        // verify must report the signature failure, or every delivery would look like a clock bug.
        let k = key();
        let other = key();
        let ts = (NOW - 86_400).to_string();
        let forged = sign(&other, &ts, BODY);
        let rejection = verify(Some(&k.public_b64), &headers(&forged, &ts), BODY, NOW, 300)
            .expect_err("forged");
        assert_eq!(rejection.reason, "telnyx_signature_verification_failed");
    }

    #[test]
    fn an_unparsable_public_key_is_a_configuration_refusal() {
        let k = key();
        let ts = NOW.to_string();
        let sig = sign(&k, &ts, BODY);
        for bad in ["not base64!!", "AAAA"] {
            let rejection = verify(Some(bad), &headers(&sig, &ts), BODY, NOW, 300)
                .expect_err("a key we cannot parse verifies nothing");
            assert_eq!(rejection.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(rejection.reason, "telnyx_public_key_invalid");
        }
    }

    #[test]
    fn the_replay_guard_refuses_a_second_use_and_forgets_old_entries() {
        let guard = ReplayGuard::default();
        assert!(guard.is_empty());
        assert!(!guard.is_replayed("sig-a", NOW, 300));
        guard.ack("sig-a", NOW, 300);
        assert!(guard.is_replayed("sig-a", NOW, 300));
        // A different signature (a different delivery) is untouched.
        assert!(!guard.is_replayed("sig-b", NOW, 300));
        // Once the window has passed, the entry is pruned and the signature is no longer remembered
        // — the guard bounds replay, it does not grow without bound.
        assert!(!guard.is_replayed("sig-a", NOW + 301, 300));
        assert!(guard.is_empty());
    }

    #[test]
    fn the_refusal_body_carries_the_reason_and_the_status() {
        // The live probe reads this shape out of the HTTP response, so it is part of the contract.
        let rejection = replayed_rejection();
        assert_eq!(rejection.status, StatusCode::UNAUTHORIZED);
        let body = format!("{:?}", rejection);
        assert!(body.contains("telnyx_signature_replayed"));
    }
}
