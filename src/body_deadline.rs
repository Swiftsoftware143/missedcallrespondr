//! Request-body read deadline (kanban t_7f688018).
//!
//! Before this module nothing bounded how long a request body might take to ARRIVE. A client could
//! send a request head with a valid `Content-Length` and then stop: the task, the connection and the
//! partially-read body buffer stayed pinned for ever. No 408, no close, no log line. Measured live
//! on this app's deployed binary `69bc5d30817332ae` (10-before.txt): 13 of 13 stalled-body legs
//! parked with no response inside a 45 s budget — the public receivers a stranger can reach
//! (`POST /api/v1/telnyx/webhook`, `POST /api/v1/webhooks/stripe`, `POST /api/v1/webhooks/paypal`),
//! the unauthenticated `POST /api/v1/internal/*` push routes (`/portfolio-companies`,
//! `/portfolio-sync`, `/tag-provision`), the `/api/v1/auth/*` receivers, and the protected surface
//! carrying a real session (`POST /api/v1/tags`, `POST /api/v1/contacts`, `PUT /api/v1/settings`).
//! WorkflowSwift closed the same hole in t_e7cba83e (408 at t+30.0 s), ADASwift in t_b3d626ed,
//! FunnelSwift in t_488a19d4 and CoreSwift-CRM in t_59745689; this is the missedcallrespondr arm of
//! that fleet-wide bound, ported from the CoreSwift-CRM file.

use axum::{
    body::{Body, Bytes},
    extract::{FromRequest, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tracing::warn;

/// Default body-read deadline: how long the body of a request that carries one may take to
/// arrive before the request is answered `408` and its task, connection and partially-read body
/// buffer are released. Override with `BODY_READ_DEADLINE_SECS` (clamped to `5..=300` in
/// `config.rs`, so neither a typo nor a fat finger can shed real traffic).
///
/// 30 s is orders of magnitude above the time a real body on these routes takes — a Telnyx event, a
/// Stripe delivery or a tag-provision push is kilobytes over a same-region link — and still generous
/// to a slow sender: a full 2 MiB body (axum's default limit, which this app does not raise
/// anywhere) may arrive as slowly as ~70 KiB/s and complete inside it.
pub const DEFAULT_BODY_READ_DEADLINE_SECS: u64 = 30;

/// Lower clamp: below this a bound is short enough to shed a legitimately slow sender.
pub const MIN_BODY_READ_DEADLINE_SECS: u64 = 5;
/// Upper clamp: above this the bound stops being a bound for a stranger-held connection.
pub const MAX_BODY_READ_DEADLINE_SECS: u64 = 300;

/// How long a request body may take to arrive, as configured.
///
/// Its own type so the middleware can be mounted (and unit-tested) without an `AppState` — and so
/// the number it enforces is the number the boot log printed, never re-derived per request.
#[derive(Clone, Copy, Debug)]
pub struct BodyReadDeadline(std::time::Duration);

impl BodyReadDeadline {
    /// From the configured seconds. Clamping lives in `config.rs`; this is a plain carrier.
    pub fn from_secs(seconds: u64) -> Self {
        Self(std::time::Duration::from_secs(seconds))
    }

    /// The deadline as a duration.
    pub fn duration(self) -> std::time::Duration {
        self.0
    }
}

/// The `408` a request whose body never finished arriving is answered with.
///
/// Carries the deadline so a client log says which bound was hit, and `Connection: close` because
/// the declared body was never consumed: this connection cannot be reused for another request.
fn body_deadline_response(deadline: std::time::Duration) -> Response {
    Response::builder()
        .status(StatusCode::REQUEST_TIMEOUT)
        .header("Connection", "close")
        .body(Body::from(format!(
            "{{\"error\":\"Request body was not received in time. Retry.\",\"status\":408,\
             \"body_deadline_seconds\":{}}}",
            deadline.as_secs()
        )))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::REQUEST_TIMEOUT)
                .header("Connection", "close")
                .body(Body::empty())
                .unwrap_or_else(|_| Response::new(Body::empty()))
        })
}

/// Middleware: bound how long a request BODY may take to arrive, and answer `408` if it has not
/// finished within [`BodyReadDeadline`] of the headers.
///
/// **Why a body deadline and not a handler budget** (kanban t_7f688018). These routes do their own
/// credential or key work *after* the body has been read: the Telnyx, Stripe and PayPal receivers
/// verify a signature over the raw bytes, `/api/v1/internal/*` checks `X-Internal-Key` inside the
/// handler, `/api/v1/auth/login` hashes a password against the stored hash. A wall-clock budget over
/// those routes would have to be sized for that work plus whatever the handler dispatches, and
/// elapsing it drops work that was legitimately in progress — for a captured call or a verified
/// payment that is a LOST EVENT, not a retried login. This middleware bounds only the thing that is
/// actually unbounded: a client that sends a request head and then stops. A handler that legitimately
/// takes seconds is untouched, because the deadline is over by the time it runs.
///
/// **The deadline is total, measured from the headers** — not a per-chunk idle timeout that resets
/// on every frame (`tower_http::timeout::RequestBodyTimeoutLayer` works that way). A resetting
/// timeout is not a bound here at all: a client that dribbles one byte every N-1 seconds holds the
/// task, the connection and the buffer for ever. On elapse the inner future is dropped, so the
/// partially-read body goes with it, the request is answered `408`, and the event is logged.
///
/// **What a legitimately slow sender gets.** A body that *is* arriving is read at full speed; the
/// deadline only fires on one that has stopped. If a body really does exceed the deadline the
/// request is answered `408` and closed, and the senders of these routes (Telnyx, Stripe, PayPal,
/// FunnelSwift, WorkflowSwift) retry their deliveries — at that point the sender had stopped
/// mid-body, so the event was already undeliverable.
///
/// On success the bytes read here are handed to the inner service as an already-complete body, so
/// the handler's own extractor (`Json`, `Bytes`, `Multipart`) sees exactly the bytes the client
/// sent — same bytes, same headers, same limit, because the read below *is* the same extractor: a
/// body over axum's `DefaultBodyLimit` is rejected through the identical code path, with the
/// identical `413`. That is what keeps the webhook HMACs verifiable: the signature is computed over
/// the raw bytes, and these are the raw bytes.
pub async fn body_read_deadline_middleware(
    State(deadline): State<BodyReadDeadline>,
    request: Request,
    next: Next,
) -> Response {
    let deadline = deadline.duration();

    // Scope (the same expression the FunnelSwift arm of this bound uses, t_488a19d4): the deadline
    // engages only for a request that DECLARES a body on a method that can carry one. Everything
    // else is handed to the inner service with its original body, untouched — so a route that reads
    // no body (a GET, a bodyless POST) is not changed by this layer at all: with no declared body
    // there is nothing that can be waited for, so there is nothing to bound and no behaviour to
    // alter. 20 of this app's 119 registered route entries are GET-only; the census in
    // scope-census.txt resolves 19 of their handlers and finds ZERO body extractors among them, so
    // no body-reading route here is reachable by a GET.
    if !declares_a_body(request.method(), request.headers()) {
        return next.run(request).await;
    }

    // The head is cloned rather than rebuilt so the inner request keeps the method, uri, version,
    // headers and every extension an outer layer inserted. Cloning `Parts` copies the extensions
    // map (shared handles), so nothing inserted by a layer above is lost — that is what keeps the
    // `Claims` an outer `auth_middleware` injected available to the handler on the protected
    // surface, where this layer is mounted INNERMOST and auth sits outside it.
    let (parts, body) = request.into_parts();
    let probe = Request::from_parts(parts.clone(), body);

    // `()` is the extractor state: `Bytes::from_request` takes its limit from the request's own
    // extensions (axum's `DefaultBodyLimit`), not from the state, so the limit the handler would
    // have applied is the limit applied here.
    match tokio::time::timeout(deadline, Bytes::from_request(probe, &())).await {
        Ok(Ok(bytes)) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        // Over the body limit, or a read error on the way in: exactly the rejection the handler's
        // own extractor would have produced, produced by the same extractor.
        Ok(Err(rejection)) => rejection.into_response(),
        Err(_elapsed) => {
            warn!(
                "request body not received within {:?} (stalled body) — answered 408 for {} {}",
                deadline, parts.method, parts.uri
            );
            body_deadline_response(deadline)
        }
    }
}

/// Whether the request declares a body a handler could wait for: `Content-Length` greater than
/// zero, or `Transfer-Encoding`, on a method that can carry a body.
///
/// A `Content-Length: 0` declares no body and is answered without a wait, and neither is a GET:
/// there is no declared body for a handler to block on, so bounding one would only invent a new
/// failure mode for requests that work today.
fn declares_a_body(method: &Method, headers: &HeaderMap) -> bool {
    if !matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        return false;
    }
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    declared > 0 || headers.contains_key(header::TRANSFER_ENCODING)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    // ── body-read deadline (kanban t_7f688018) ──────────────────────────────────────────────
    //
    // Three properties, and they are the ones a later refactor must not break: the deadline fires
    // on a body that never arrives; a body that does arrive reaches the handler byte for byte (the
    // webhook receivers HMAC the raw bytes, so anything else silently breaks signature
    // verification); and the middleware does not widen the limit on how much a single request may
    // buffer.

    /// A request body that produces no frame and never ends: the client-side shape of the hold
    /// this middleware bounds (head sent, body never arrives).
    struct StalledBody;

    impl futures_core::Stream for StalledBody {
        type Item = Result<Bytes, std::io::Error>;

        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            std::task::Poll::Pending
        }
    }

    /// The production mount shape: one route whose handler reads the body, wrapped by the layer.
    fn body_deadline_app(deadline: std::time::Duration) -> Router {
        Router::new()
            .route("/webhook", post(|body: Bytes| async move { body }))
            .layer(axum::middleware::from_fn_with_state(
                BodyReadDeadline(deadline),
                body_read_deadline_middleware,
            ))
    }

    /// A stalled body must end the hold: 408, `Connection: close`, promptly. Before this
    /// middleware the same request held a task, a connection and a partially-read body buffer for
    /// ever — measured live on `/api/v1/telnyx/webhook` and 12 more legs at t+45.0 s, still parked.
    #[tokio::test]
    async fn stalled_body_is_answered_408_within_the_deadline() {
        // 150 ms, not the configured 30 s: the assertion is about WHICH requests the deadline
        // fires on, not how long the operator set it to.
        let app = body_deadline_app(std::time::Duration::from_millis(150));
        let started = std::time::Instant::now();
        let resp = app
            .oneshot(
                HttpRequest::post("/webhook")
                    .header("content-length", "100000")
                    .body(Body::from_stream(StalledBody))
                    .expect("request"),
            )
            .await
            .expect("router");
        let elapsed = started.elapsed();

        assert_eq!(
            resp.status(),
            StatusCode::REQUEST_TIMEOUT,
            "a body that never arrives must be answered, not parked"
        );
        assert_eq!(
            resp.headers()
                .get("connection")
                .and_then(|v| v.to_str().ok()),
            Some("close"),
            "the declared body was never consumed, so the connection must not be reused"
        );
        let body = axum::body::to_bytes(resp.into_body(), 4096)
            .await
            .expect("body");
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("\"status\":408") && text.contains("\"body_deadline_seconds\""),
            "body was {}",
            text
        );
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "the deadline must fire promptly, took {elapsed:?}"
        );
    }

    /// The bytes the client sends are the bytes the handler sees. This is why the deadline reads
    /// the body instead of replacing it: the Telnyx, Stripe and PayPal receivers verify a signature
    /// computed over exactly these bytes, and the internal push routes parse them into records.
    #[tokio::test]
    async fn complete_body_reaches_the_handler_unchanged() {
        let app = body_deadline_app(std::time::Duration::from_secs(30));
        let payload = br#"{"event_type":"call.initiated","data":{"from":"+15550001111"}}"#;
        let resp = app
            .oneshot(
                HttpRequest::post("/webhook")
                    .header("content-type", "application/json")
                    .header("content-length", payload.len().to_string())
                    .header("telnyx-signature-ed25519", "deadbeef")
                    .body(Body::from(payload.to_vec()))
                    .expect("request"),
            )
            .await
            .expect("router");

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 4096)
            .await
            .expect("body");
        assert_eq!(
            &body[..],
            &payload[..],
            "the handler must see the raw request bytes, unmodified"
        );
    }

    /// A body over axum's own 2 MiB limit gets the extractor's own rejection: the deadline must not
    /// become a wider hole for a single unauthenticated request to pin memory. (`Bytes`/`Json` do
    /// not police a declared `Content-Length` up front — the request below really sends the bytes.)
    #[tokio::test]
    async fn body_over_the_default_limit_is_rejected_413() {
        let app = body_deadline_app(std::time::Duration::from_secs(30));
        let oversized = vec![b'x'; 2 * 1024 * 1024 + 1];
        let resp = app
            .oneshot(
                HttpRequest::post("/webhook")
                    .header("content-length", oversized.len().to_string())
                    .body(Body::from(oversized))
                    .expect("request"),
            )
            .await
            .expect("router");
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// A request that declares NO body is passed through untouched: with no declared body there is
    /// nothing that can be waited for, so the layer must not introduce a wait of its own. This is
    /// the scope leg — it is what lets the bodyless routes on both surfaces carry the layer without
    /// their contract changing, and it is measured live in both phases (legs N1/N2/N3 and S1).
    #[tokio::test]
    async fn request_without_a_declared_body_is_not_bounded() {
        let app = Router::new()
            .route("/read", axum::routing::get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                BodyReadDeadline(std::time::Duration::from_millis(150)),
                body_read_deadline_middleware,
            ));
        let started = std::time::Instant::now();
        // A body stream that never produces a frame, and no `Content-Length`: exactly the client
        // shape that would park a body-reading route.
        let resp = app
            .clone()
            .oneshot(
                HttpRequest::get("/read")
                    .body(Body::from_stream(StalledBody))
                    .expect("request"),
            )
            .await
            .expect("router");
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(100),
            "a request that declares no body must not be waited for"
        );

        // …and neither is a request that declares an EMPTY body.
        let resp = app
            .oneshot(
                HttpRequest::post("/read")
                    .header("content-length", "0")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("router");
        assert_ne!(resp.status(), StatusCode::REQUEST_TIMEOUT);
    }
}
