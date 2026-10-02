use axum::{
    extract::{Extension, Path, State},
    Json,
};
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

/// `POST /api/v1/messages` — the tenant console's "Send Message" form is its ONLY caller.
///
/// NOTHING IN THIS SERVICE TRANSMITS A MESSAGE (kanban t_4bcf81a8, measured 2026-10-02): the only
/// Telnyx call in the repo is `/v2/phone_numbers` (buy / release), there is no `/v2/messages`, no
/// Twilio client and no outbound queue, and `telnyx_config` (a credential entered by an admin at
/// `POST /api/v1/admin/telnyx-config`) is read by no send path. So this route RECORDS a message in
/// the tenant's own log. It used to write `status = "sent"` + `sent_at = now()` for a purely
/// client-supplied `from_number`, so the console painted a green "sent" badge for a text that was
/// never sent, from a number the tenant may not even own.
///
/// Two things now hold, and both are asserted live by the t_4bcf81a8 probe:
///
/// 1. `status = "logged"` and `sent_at = NULL` — the row says exactly what happened (recorded, not
///    delivered). Wiring a real transport means adding the provider call that sets the status from
///    the provider's own answer (`queued` / `sent` / `delivered`); until that call exists the status
///    must not claim delivery.
/// 2. The sender is scoped. An `outbound` message may only leave from a number the caller holds, and
///    an `inbound` message may only arrive at one — the same ACTIVE-row predicate
///    [`crate::handlers::telnyx_handler::list_numbers`] uses, so a released number is not a sender
///    either. Anything else is a 400 naming the field. The comparison is on digits, so formatting
///    ("(555) 010-1234" vs "+15550101234") is not a refusal reason, but the value must match a held
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
    let id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO messages (id, call_id, direction, from_number, to_number, body, status, sent_at, tenant_id, created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(req.call_id)
    .bind(&direction)
    .bind(req.from_number.trim())
    .bind(req.to_number.trim())
    .bind(&req.body)
    .bind("logged")
    .bind(None::<chrono::NaiveDateTime>)
    .bind(claims.aid)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let item = sqlx::query_as::<_, Message>("SELECT * FROM messages WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(item))
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
