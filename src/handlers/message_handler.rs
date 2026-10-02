use axum::{
    extract::{Extension, Path, State},
    Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::features;
use crate::{
    config::Claims,
    error::AppError,
    models::message::{CreateMessageRequest, Message},
    state::AppState,
};

pub async fn list_messages(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> Result<Json<Vec<Message>>, AppError> {
    let items = sqlx::query_as::<_, Message>(
        "SELECT * FROM messages WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(claims.aid)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(items))
}

// ---------------------------------------------------------------------------
// The outbound transport (kanban t_2ed95642)
// ---------------------------------------------------------------------------

/// Where the Telnyx messaging API lives.
///
/// `TELNYX_API_BASE` overrides it exactly the way this app already lets `PAYPAL_API_BASE` point its
/// PayPal verify call at a sandbox: the DEFAULT is the production host, so an unset environment
/// behaves byte-identically to before, and an acceptance run can aim the transport at a local stub
/// and prove the whole path — including a provider failure — without ever holding a real
/// credential (`payment-arm-live-proof-with-stub`). Read per call, so the knob needs no rebuild.
const DEFAULT_TELNYX_API_BASE: &str = "https://api.telnyx.com";

fn telnyx_api_base() -> String {
    std::env::var("TELNYX_API_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_TELNYX_API_BASE.to_string())
}

/// Map Telnyx's OWN per-recipient status word onto this app's stored vocabulary.
///
/// The vocabulary is the provider's, not ours: `queued` (accepted, nothing further promised),
/// `sent` (handed to the carrier) and `delivered` (the recipient's network confirmed it) come
/// straight from Telnyx's `to[].status` on the send answer and on the `message.sent` /
/// `message.finalized` events. Anything that is a failure in Telnyx's own words becomes `failed`.
/// `None` means "a word this app does not know" — the caller decides (see
/// [`outcome_from_provider`]), it is never silently treated as delivery.
pub(crate) fn map_provider_status(word: &str) -> Option<(&'static str, bool, bool)> {
    match word.trim().to_ascii_lowercase().as_str() {
        "delivered" => Some(("delivered", true, true)),
        "sent" | "sending" => Some(("sent", true, false)),
        "queued" | "accepted" | "scheduled" => Some(("queued", false, false)),
        "delivery_failed"
        | "delivery_unconfirmed"
        | "failed"
        | "rejected"
        | "expired"
        | "undelivered" => Some(("failed", false, false)),
        _ => None,
    }
}

/// What the provider's own answer means for the row this route is about to store.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ProviderOutcome {
    /// Telnyx's message id — the key later delivery events match on.
    pub provider_message_id: Option<String>,
    /// The stored `messages.status`: `queued` / `sent` / `delivered` / `failed` (`logged` for the
    /// inbound arm, which is not transmitted at all).
    pub status: &'static str,
    pub mark_sent: bool,
    pub mark_delivered: bool,
    /// The provider's own words, set exactly when `status == "failed"`.
    pub detail: Option<String>,
}

/// The provider's own sentence about a refusal, so the tenant reads Telnyx's reason and not ours.
///
/// Telnyx answers errors as `{"errors":[{"detail":…}]}`; the shapes below cover that plus a bare
/// `{"message":…}`. When none of them is present the body itself is the reason — an unparsed body
/// is still evidence, and inventing a friendlier sentence here would hide it.
fn provider_detail(body: &Value, http_status: u16) -> String {
    let detail = body
        .pointer("/errors/0/detail")
        .and_then(|v| v.as_str())
        .or_else(|| body.pointer("/errors/0/title").and_then(|v| v.as_str()))
        .or_else(|| {
            body.pointer("/data/errors/0/detail")
                .and_then(|v| v.as_str())
        })
        .or_else(|| body.pointer("/message").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let raw = body.to_string();
            if raw == "null" || raw.is_empty() {
                "no response body".to_string()
            } else {
                raw
            }
        });
    format!("Telnyx answered {}: {}", http_status, detail)
}

/// Turn the provider's answer into the row's meaning. THE RULE (kanban t_2ed95642): the stored
/// status is derived from the provider's own answer, never from the request.
///
/// * Any non-2xx is a `failed` row carrying the provider's own reason — a 4xx from Telnyx is a
///   refusal, not a delivery.
/// * A 2xx is an ACCEPTED send: the status is the provider's own `to[].status` word when this app
///   knows it, and `queued` when it does not (a word this app has not seen is still an acceptance,
///   and `queued` claims nothing beyond that). It is only ever `delivered` when the provider says
///   so — `sent_at` and `delivered_at` follow the same rule.
/// * A 2xx with no message id is still `queued` (the provider accepted it), and the missing id is
///   logged: with no id no later event can find the row, which is a fact an operator needs.
pub(crate) fn outcome_from_provider(http_status: u16, body: &Value) -> ProviderOutcome {
    if !(200..300).contains(&http_status) {
        return ProviderOutcome {
            provider_message_id: None,
            status: "failed",
            mark_sent: false,
            mark_delivered: false,
            detail: Some(provider_detail(body, http_status)),
        };
    }

    let provider_message_id = body
        .pointer("/data/id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if provider_message_id.is_none() {
        tracing::warn!(
            "Telnyx accepted a message without returning a message id (body={}); no delivery event can be matched to it",
            body
        );
    }

    let word = body
        .pointer("/data/to/0/status")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match map_provider_status(word) {
        Some(("failed", _, _)) => ProviderOutcome {
            provider_message_id,
            status: "failed",
            mark_sent: false,
            mark_delivered: false,
            detail: Some(format!(
                "Telnyx answered {}: the message was not accepted (status \"{}\")",
                http_status,
                word.trim()
            )),
        },
        Some((status, mark_sent, mark_delivered)) => ProviderOutcome {
            provider_message_id,
            status,
            mark_sent,
            mark_delivered,
            detail: None,
        },
        // Accepted by the provider, in a word this app does not know yet: `queued` is the honest
        // reading (it is with the provider) and claims nothing about the handset.
        None => ProviderOutcome {
            provider_message_id,
            status: "queued",
            mark_sent: false,
            mark_delivered: false,
            detail: None,
        },
    }
}

/// Hand one message to Telnyx and report what its answer means.
///
/// Runs AFTER the sender check, the plan gate and the configuration check: nothing is transmitted
/// for a request that would be refused, and nothing is transmitted before it is paid for.
async fn deliver_outbound(
    api_base: &str,
    api_key: &str,
    messaging_profile_id: &str,
    from: &str,
    to: &str,
    body: &str,
) -> ProviderOutcome {
    let client = reqwest::Client::new();
    let sent = client
        .post(format!("{}/v2/messages", api_base))
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&json!({
            "from": from,
            "to": to,
            "text": body,
            "messaging_profile_id": messaging_profile_id,
        }))
        .send()
        .await;

    match sent {
        Ok(resp) => {
            let http_status = resp.status().as_u16();
            // A body that is not JSON is still the provider's answer: keep the status code as the
            // reason rather than pretending we heard nothing.
            let answer: Value = resp.json().await.unwrap_or(Value::Null);
            outcome_from_provider(http_status, &answer)
        }
        // Not reached at all (DNS, TLS, timeout): there is no provider answer, so the row may not
        // claim an acceptance either.
        Err(e) => ProviderOutcome {
            provider_message_id: None,
            status: "failed",
            mark_sent: false,
            mark_delivered: false,
            detail: Some(format!("could not reach Telnyx: {}", e)),
        },
    }
}

/// `POST /api/v1/messages` — the tenant console's "Send Message" form is its ONLY caller.
///
/// An `outbound` message is TRANSMITTED here: Telnyx's `POST /v2/messages` is called with the
/// stored `api_key` + `messaging_profile_id` and the caller's own number as `from` (kanban
/// t_2ed95642). The row is then derived from the PROVIDER'S OWN ANSWER, never from the request —
/// `queued` when Telnyx accepts it, `sent` / `delivered` only when Telnyx says so (`delivered_at`
/// stays NULL until a delivery event says otherwise; the `message.*` events land on
/// [`crate::handlers::telnyx_handler::webhook`] and are matched by `provider_message_id`), and a
/// provider failure is a `failed` row plus a 424 carrying the provider's own words (424, not 502:
/// measured — Cloudflare replaces an origin 502 with its own page, so the provider's sentence would
/// never reach the operator; see [`crate::error::AppError::UpstreamRefused`]). Nothing here
/// can leave a row claiming a delivery that the provider did not report.
///
/// An `inbound` message is a record of something that ARRIVED; this route transmits nothing for it
/// and stores `status = "logged"` with `sent_at` NULL, which is exactly what happened.
///
/// Two further things hold, and both are asserted live by this card's probe:
///
/// 1. `telnyx_config` empty (no `api_key`, or no `messaging_profile_id`) is a 503 naming the
///    missing configuration and writes NO row — nothing was attempted, so there is nothing to
///    record. A 500 here would blame the caller's request for an operator's gap.
/// 2. The sender is scoped. An `outbound` message may only leave from a number the caller holds,
///    and an `inbound` message may only arrive at one — the same ACTIVE-row predicate
///    [`crate::handlers::telnyx_handler::list_numbers`] uses, so a released number is not a sender
///    either. Anything else is a 400 naming the field. The comparison is on digits, so formatting
///    ("(555) 010-1234" vs "+155****1234") is not a refusal reason, but the value must match a held
///    number and carry at least 7 digits, so an empty or garbage string can never match.
pub async fn create_message(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Json(req): Json<CreateMessageRequest>,
) -> Result<Json<Message>, AppError> {
    let tenant_id: Uuid = claims.aid;

    // Which end of the message must be the caller's own number? Outbound leaves FROM it, inbound
    // arrives AT it. A direction that is neither is refused rather than stored as free text.
    let direction = req.direction.trim().to_ascii_lowercase();
    let (own_field, own_number) = match direction.as_str() {
        "outbound" => ("from_number", req.from_number.trim()),
        "inbound" => ("to_number", req.to_number.trim()),
        _ => {
            return Err(AppError::BadRequest(format!(
                "direction must be \"outbound\" or \"inbound\" (got \"{}\")",
                req.direction
            )))
        }
    };

    let held: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM phone_numbers
         WHERE tenant_id = $1
           AND is_active = true
           AND (provider = 'telnyx' OR telnyx_connection_id IS NOT NULL)
           AND length(regexp_replace($2, '[^0-9]', '', 'g')) >= 7
           AND regexp_replace(number, '[^0-9]', '', 'g') = regexp_replace($2, '[^0-9]', '', 'g')",
    )
    .bind(tenant_id)
    .bind(own_number)
    .fetch_one(&state.pool)
    .await?;

    if held == 0 {
        return Err(AppError::BadRequest(format!(
            "{} \"{}\" is not one of this account's active numbers",
            own_field, own_number
        )));
    }

    features::enforce_feature_limit(&state.pool, tenant_id, "max_messages", "Messages").await?;

    let is_outbound = direction == "outbound";
    let outcome = if is_outbound {
        // The credential and the messaging profile are an OPERATOR's responsibility. With either
        // missing there is nothing to send with: refuse, name what is missing, and write no row —
        // an entry in the log would imply an attempt that never happened.
        let conf = crate::handlers::telnyx_handler::get_telnyx_config(&state.pool)
            .await?
            .filter(|c| {
                !c.api_key.trim().is_empty()
                    && c.messaging_profile_id
                        .as_deref()
                        .map(|m| !m.trim().is_empty())
                        .unwrap_or(false)
            })
            .ok_or_else(|| {
                AppError::ServiceUnavailable(
                    "Text delivery is not configured: an administrator must save the Telnyx API key \
                     and the messaging profile id of a messaging-enabled number (Admin, Telnyx \
                     Config) before messages can be sent."
                        .to_string(),
                )
            })?;
        let profile = conf
            .messaging_profile_id
            .as_deref()
            .unwrap_or_default()
            .trim();
        deliver_outbound(
            &telnyx_api_base(),
            conf.api_key.trim(),
            profile,
            own_number,
            req.to_number.trim(),
            &req.body,
        )
        .await
    } else {
        ProviderOutcome {
            provider_message_id: None,
            status: "logged",
            mark_sent: false,
            mark_delivered: false,
            detail: None,
        }
    };

    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO messages (id, call_id, direction, from_number, to_number, body, status, sent_at, delivered_at, provider_message_id, tenant_id, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(id)
    .bind(req.call_id)
    .bind(&direction)
    .bind(req.from_number.trim())
    .bind(req.to_number.trim())
    .bind(&req.body)
    .bind(outcome.status)
    .bind(if outcome.mark_sent { Some(now) } else { None })
    .bind(if outcome.mark_delivered { Some(now) } else { None })
    .bind(outcome.provider_message_id.as_deref())
    .bind(claims.aid)
    .bind(now)
    .execute(&state.pool)
    .await?;

    // The failed attempt is recorded BEFORE the caller is told, so the tenant's log carries what
    // happened either way, and the answer to the caller repeats the provider's own reason.
    if outcome.status == "failed" {
        let detail = outcome
            .detail
            .unwrap_or_else(|| "the provider refused the message".to_string());
        tracing::warn!("message {} was not sent: {}", id, detail);
        return Err(AppError::UpstreamRefused(detail));
    }

    let item = sqlx::query_as::<_, Message>("SELECT * FROM messages WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
}

// ---------------------------------------------------------------------------
// The automatic arm (kanban t_31f9cf38): a response rule's `sms` action
// ---------------------------------------------------------------------------

/// Text the caller back on behalf of a response rule — the same transport, the same row and the
/// same provider-derived status as the console's Send Message form, with two differences:
///
/// 1. There is no request to validate. The sender is the number the caller dialed, which is the
///    tenant's own ACTIVE number — it is what resolved the tenant in the inbound webhook in the
///    first place — and the recipient is the caller.
/// 2. There is no caller to hand a status code to: nobody is watching the ring. The outcome of the
///    attempt therefore reaches the operator as the ROW in the Messages log (queued / sent /
///    delivered / failed), exactly as it does for a manual send.
///
/// A missing credential keeps the manual arm's contract: nothing was attempted, so NO row is
/// written — it is returned as an error for the caller to log, and the console's Response Rules
/// screen and the guide both say a text needs the Telnyx configuration.
///
/// The plan's message limit is NOT enforced here. The manual form refuses over-limit sends with a
/// prompt the operator can act on; silently dropping a missed-call reply mid-ring would break the
/// product's one promise with nothing to show for it. The row still counts towards the tenant's
/// total (the plan counter reads this table).
pub(crate) async fn send_rule_sms(
    state: &AppState,
    tenant_id: Uuid,
    call_id: Uuid,
    from: &str,
    to: &str,
    body: &str,
    rule_name: &str,
) -> Result<Uuid, AppError> {
    let conf = crate::handlers::telnyx_handler::get_telnyx_config(&state.pool)
        .await?
        .filter(|c| {
            !c.api_key.trim().is_empty()
                && c.messaging_profile_id
                    .as_deref()
                    .map(|m| !m.trim().is_empty())
                    .unwrap_or(false)
        })
        .ok_or_else(|| {
            AppError::ServiceUnavailable(
                "Text delivery is not configured: an administrator must save the Telnyx API key \
                 and the messaging profile id of a messaging-enabled number (Admin, Telnyx Config) \
                 before a response rule can text a caller back."
                    .to_string(),
            )
        })?;

    let profile = conf
        .messaging_profile_id
        .as_deref()
        .unwrap_or_default()
        .trim();

    let outcome = deliver_outbound(
        &telnyx_api_base(),
        conf.api_key.trim(),
        profile,
        from,
        to,
        body,
    )
    .await;

    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO messages (id, call_id, direction, from_number, to_number, body, status, sent_at, delivered_at, provider_message_id, tenant_id, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(id)
    .bind(Some(call_id))
    .bind("outbound")
    .bind(from)
    .bind(to)
    .bind(body)
    .bind(outcome.status)
    .bind(if outcome.mark_sent { Some(now) } else { None })
    .bind(if outcome.mark_delivered { Some(now) } else { None })
    .bind(outcome.provider_message_id.as_deref())
    .bind(tenant_id)
    .bind(now)
    .execute(&state.pool)
    .await?;

    if outcome.status == "failed" {
        tracing::warn!(
            "response rule \"{}\": the automatic text to {} was refused: {}",
            rule_name,
            to,
            outcome
                .detail
                .clone()
                .unwrap_or_else(|| "the provider refused the message".to_string())
        );
    }

    Ok(id)
}

pub async fn get_message(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Message>, AppError> {
    let item =
        sqlx::query_as::<_, Message>("SELECT * FROM messages WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(claims.aid)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::NotFound("Message not found".into()))?;
    Ok(Json(item))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_words_map_onto_the_stored_vocabulary() {
        assert_eq!(
            map_provider_status("queued"),
            Some(("queued", false, false))
        );
        assert_eq!(
            map_provider_status(" QUEUED "),
            Some(("queued", false, false))
        );
        assert_eq!(
            map_provider_status("accepted"),
            Some(("queued", false, false))
        );
        assert_eq!(map_provider_status("sent"), Some(("sent", true, false)));
        assert_eq!(map_provider_status("sending"), Some(("sent", true, false)));
        assert_eq!(
            map_provider_status("delivered"),
            Some(("delivered", true, true))
        );
        assert_eq!(
            map_provider_status("delivery_failed"),
            Some(("failed", false, false))
        );
        assert_eq!(
            map_provider_status("expired"),
            Some(("failed", false, false))
        );
        // A word this app has never seen is NOT a delivery and NOT a failure.
        assert_eq!(map_provider_status("some_new_word"), None);
        assert_eq!(map_provider_status(""), None);
    }

    #[test]
    fn an_accepted_send_is_queued_and_keeps_the_id() {
        let body = json!({"data": {"id": "m-1", "to": [{"status": "queued"}]}});
        let out = outcome_from_provider(200, &body);
        assert_eq!(out.provider_message_id.as_deref(), Some("m-1"));
        assert_eq!(out.status, "queued");
        assert!(!out.mark_sent);
        assert!(!out.mark_delivered);
        assert_eq!(out.detail, None);
    }

    #[test]
    fn only_the_providers_own_delivered_word_marks_delivery() {
        let delivered = json!({"data": {"id": "m-2", "to": [{"status": "delivered"}]}});
        let out = outcome_from_provider(200, &delivered);
        assert_eq!(out.status, "delivered");
        assert!(out.mark_sent);
        assert!(out.mark_delivered);

        let sent = json!({"data": {"id": "m-3", "to": [{"status": "sent"}]}});
        let out = outcome_from_provider(200, &sent);
        assert_eq!(out.status, "sent");
        assert!(out.mark_sent);
        assert!(!out.mark_delivered);
    }

    #[test]
    fn an_unknown_provider_word_is_still_an_accepted_send() {
        let body = json!({"data": {"id": "m-4", "to": [{"status": "in_some_new_state"}]}});
        let out = outcome_from_provider(200, &body);
        assert_eq!(out.status, "queued");
        assert_eq!(out.provider_message_id.as_deref(), Some("m-4"));
        assert!(!out.mark_delivered);
        assert_eq!(out.detail, None);
    }

    #[test]
    fn a_provider_refusal_is_a_failed_row_with_the_providers_words() {
        let body = json!({"errors": [{"detail": "Invalid API key", "title": "Unauthorized"}]});
        let out = outcome_from_provider(401, &body);
        assert_eq!(out.status, "failed");
        assert_eq!(out.provider_message_id, None);
        assert!(!out.mark_sent);
        assert!(!out.mark_delivered);
        let detail = out.detail.expect("a refusal carries the provider's reason");
        assert!(detail.contains("401"), "detail was {detail}");
        assert!(detail.contains("Invalid API key"), "detail was {detail}");
    }

    #[test]
    fn an_unparsable_refusal_body_is_still_reported() {
        let out = outcome_from_provider(500, &Value::Null);
        assert_eq!(out.status, "failed");
        assert!(out.detail.unwrap().contains("500"));
    }

    #[test]
    fn a_failure_reported_inside_a_2xx_is_the_providers_own_verdict() {
        let body = json!({"data": {"id": "m-5", "to": [{"status": "delivery_failed"}]}});
        let out = outcome_from_provider(200, &body);
        assert_eq!(out.status, "failed");
        assert!(!out.mark_sent);
        assert!(!out.mark_delivered);
        assert!(out.detail.unwrap().contains("delivery_failed"));
    }

    #[test]
    fn an_accepted_send_without_an_id_is_still_queued() {
        let body = json!({"data": {"to": [{"status": "queued"}]}});
        let out = outcome_from_provider(200, &body);
        assert_eq!(out.status, "queued");
        assert_eq!(out.provider_message_id, None);
    }

    #[test]
    fn the_api_base_defaults_to_the_production_host() {
        // Safety: single-threaded test binary; no other test in this module reads TELNYX_API_BASE.
        std::env::remove_var("TELNYX_API_BASE");
        assert_eq!(telnyx_api_base(), "https://api.telnyx.com");
        std::env::set_var("TELNYX_API_BASE", "http://127.0.0.1:9/stub/");
        assert_eq!(telnyx_api_base(), "http://127.0.0.1:9/stub");
        std::env::remove_var("TELNYX_API_BASE");
        assert_eq!(telnyx_api_base(), "https://api.telnyx.com");
    }
}
