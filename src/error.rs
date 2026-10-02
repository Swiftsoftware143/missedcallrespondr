use axum::{
    body::{to_bytes, Body},
    extract::Request,
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Unauthorized(String),
    NotFound(String),
    Internal(String),
    Conflict(String),
    UpgradeRequired(String),
    /// Well-formed JSON whose *content* is invalid — the one refusal the request-body layer cannot
    /// make for us. Added by kanban t_54b1ffab for the email-format boundary: 422 with
    /// `{"error": "email: …"}`. The distinction from `BadRequest` is deliberate and narrow: the
    /// extractor rejections collapsed onto 400 by [`rejection_as_json`] are *malformed requests*
    /// (unparsable JSON, wrong field type); this variant is a request that parsed fine and named a
    /// field whose value is semantically impossible. Never return it with a `text/plain` body —
    /// every frontend does `await r.json()` before checking `r.ok`.
    Unprocessable(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            AppError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, msg),
            AppError::UpgradeRequired(msg) => (StatusCode::PAYMENT_REQUIRED, msg),
            AppError::Unprocessable(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg),
        };
        (status, Json(json!({"error": message}))).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    /// A database failure is an OPERATOR problem, never the caller's — so the real error (with its
    /// constraint name, column names, SQLSTATE) is logged and the client gets a sentence that names
    /// nothing about the schema.
    ///
    /// This used to interpolate the sqlx error straight into the 500 body, which is how
    /// `duplicate key value violates unique constraint "phone_numbers_number_key"` reached a tenant
    /// on `POST /api/v1/telnyx/numbers` (kanban t_4c15d597, measured live). The fleet answers this
    /// class generically everywhere else (FunnelSwift/ADASwift/multi-directory/WorkflowSwift all log
    /// and return `"Database error"`); this app now does too. The specific path that produced the
    /// leak is fixed at its source as well — see `telnyx_handler::purchase_number` (a released row is
    /// revived, not re-inserted), so this mapping is the class-wide backstop, not the only fix.
    fn from(e: sqlx::Error) -> Self {
        tracing::error!("Database error: {}", e);
        AppError::Internal("Database error".to_string())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::Internal(format!("Error: {}", e))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// axum extractor rejections → this app's own error shape
// ─────────────────────────────────────────────────────────────────────────────

/// Server refusals that axum's `Json` extractor produces with a **`text/plain`** body:
/// 415 (wrong content type), 400 (body is not JSON at all), 422 (JSON that does not
/// deserialize into the target type).
///
/// `www-admin/index.html`'s fetch helper is
/// `const handle = async (r) => { const d = await r.json(); if (!r.ok) throw ... }` —
/// `await r.json()` runs *before* the `!r.ok` check, so a plain-text body throws a
/// `SyntaxError` and the caller's generic `catch (e) { console.error(e) }` swallows it.
/// The admin then sees **nothing at all** for a plain client-side mistake.
const REJECTION_STATUSES: [StatusCode; 3] = [
    StatusCode::BAD_REQUEST,
    StatusCode::UNSUPPORTED_MEDIA_TYPE,
    StatusCode::UNPROCESSABLE_ENTITY,
];

/// Framing axum puts around the part the caller actually needs (the offending field, and
/// the byte position for malformed JSON). Dropped so `error` reads like every other message
/// this app returns; anything unrecognised is passed through untouched.
const EXTRACTOR_PREFIXES: [&str; 2] = [
    "Failed to deserialize the JSON body into the target type: ",
    "Failed to parse the request body as JSON: ",
];

/// Largest rejection body we will buffer before giving up and answering generically.
const REJECTION_BODY_LIMIT: usize = 64 * 1024;

/// Rewrite every extractor rejection into `400 application/json {"error":"…"}`.
///
/// One layer on the shared router, so all handlers (and every future one) inherit it and
/// nothing per-handler has to remember. Two deliberate choices:
///  * **400 for all three statuses.** A 422 is not produced by any handler in this repo —
///    it is the extractor's, and this app's bad-request convention is
///    `400 {"error": "<field> is required"}` (see `handlers/email_templates_handler.rs`).
///    Collapsing 415/422 onto 400 gives the whole API one readable bad-request shape.
///  * **Nothing else is touched.** Only a 400/415/422 whose `Content-Type` is *not* JSON is
///    rewritten, so handler-written errors (already JSON) and every successful response
///    pass through byte-identical, with no body buffering on the success path.
pub async fn rejection_as_json(req: Request, next: Next) -> Response {
    let res = next.run(req).await;

    if !REJECTION_STATUSES.contains(&res.status()) || is_json_response(&res) {
        return res;
    }

    let status = res.status();
    let (_, body) = res.into_parts();
    let message = match to_bytes(body, REJECTION_BODY_LIMIT).await {
        Ok(bytes) => extractor_message(&bytes, status),
        Err(_) => format!("request body could not be read (HTTP {})", status.as_u16()),
    };

    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

fn is_json_response(res: &Response<Body>) -> bool {
    res.headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.trim_start()
                .to_ascii_lowercase()
                .starts_with("application/json")
        })
}

fn extractor_message(bytes: &[u8], status: StatusCode) -> String {
    let raw = String::from_utf8_lossy(bytes);
    let raw = raw.trim();

    if raw.is_empty() {
        return match status {
            StatusCode::UNSUPPORTED_MEDIA_TYPE => {
                "Expected request with `Content-Type: application/json`".to_string()
            }
            _ => "invalid request body".to_string(),
        };
    }

    match EXTRACTOR_PREFIXES
        .iter()
        .find_map(|prefix| raw.strip_prefix(prefix))
    {
        Some(rest) if !rest.trim().is_empty() => rest.trim().to_string(),
        _ => raw.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_extractors_framing_and_keeps_the_field() {
        assert_eq!(
            extractor_message(
                b"Failed to deserialize the JSON body into the target type: template_type: \
                  invalid type: integer `123`, expected a string at line 1 column 45",
                StatusCode::UNPROCESSABLE_ENTITY
            ),
            "template_type: invalid type: integer `123`, expected a string at line 1 column 45"
        );
        assert_eq!(
            extractor_message(
                b"Failed to parse the request body as JSON: name: EOF while parsing a value \
                  at line 1 column 8",
                StatusCode::BAD_REQUEST
            ),
            "name: EOF while parsing a value at line 1 column 8"
        );
    }

    #[test]
    fn unknown_and_empty_bodies_still_answer_something_useful() {
        assert_eq!(
            extractor_message(
                b"Something axum changed its mind about",
                StatusCode::BAD_REQUEST
            ),
            "Something axum changed its mind about"
        );
        assert_eq!(
            extractor_message(b"  ", StatusCode::UNSUPPORTED_MEDIA_TYPE),
            "Expected request with `Content-Type: application/json`"
        );
        assert_eq!(
            extractor_message(b"", StatusCode::BAD_REQUEST),
            "invalid request body"
        );
    }

    /// The re-acquire 500 (kanban t_4c15d597) put the driver's own text in the response body,
    /// constraint name included. Whatever the database says, the client must get a sentence that
    /// names nothing about the schema — measured live before this mapping changed, pinned here.
    #[test]
    fn a_database_error_names_no_constraint_in_the_body() {
        let raw = "error returned from database: duplicate key value violates unique constraint \
                   \"phone_numbers_number_key\"";
        match AppError::from(sqlx::Error::Protocol(raw.to_string())) {
            AppError::Internal(msg) => {
                assert_eq!(msg, "Database error");
                assert!(!msg.contains("constraint"), "{msg}");
                assert!(!msg.contains("phone_numbers"), "{msg}");
            }
            other => panic!("a database failure must map to Internal, got {other:?}"),
        }
    }

    #[test]
    fn only_the_three_extractor_statuses_are_candidates() {
        for status in REJECTION_STATUSES {
            assert!([400, 415, 422].contains(&status.as_u16()), "{status}");
        }
        assert!(!REJECTION_STATUSES.contains(&StatusCode::NOT_FOUND));
        assert!(!REJECTION_STATUSES.contains(&StatusCode::UNAUTHORIZED));
        assert!(!REJECTION_STATUSES.contains(&StatusCode::METHOD_NOT_ALLOWED));
        assert!(!REJECTION_STATUSES.contains(&StatusCode::OK));
    }
}
