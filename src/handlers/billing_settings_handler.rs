//! Platform billing settings (admin): how a customer pays while card payments are off.
//!
//! Routes:
//!   GET /api/v1/admin/billing-settings — the operator's saved words (empty string = feature off)
//!   PUT /api/v1/admin/billing-settings — save them (`{"payment_instructions": "..."}`)
//!
//! Mounted under `/api/v1/admin/`, so `auth::middleware`'s platform-admin gate covers both: a
//! workspace owner is refused here exactly like every other admin route.
//!
//! Why this exists: with no `payment_providers` row and no Stripe key, `POST
//! /api/v1/checkout/create` refuses every paid plan (503 `payment_provider_not_configured`) and the
//! plan REQUEST is the buying path. The customer was told a person would reply and never how to
//! actually pay. FunnelSwift and IncentiveSwift closed this gap on 2026-11-16; this is the
//! MissedCall side.
//!
//! The stored value is read in three places, all through `billing_instructions::load`: the
//! customer's till status (`GET /api/v1/checkout/status`), the plan-request receipt mail, and this
//! screen.

use crate::billing_instructions;
use crate::config::Claims;
use crate::error::{ApiResult, AppError};
use crate::state::AppState;
use axum::{extract::State, Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};

/// GET /api/v1/admin/billing-settings
pub async fn get_billing_settings(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    let saved = billing_instructions::load(&state.pool).await;
    Ok(Json(json!({
        "payment_instructions": saved.clone().unwrap_or_default(),
        "payment_instructions_set": saved.is_some(),
        "max_length": billing_instructions::MAX_LEN,
        "message": if saved.is_some() {
            "A customer who asks to buy is shown these words, and they are printed in the receipt email."
        } else {
            "Nothing is shown to a customer who asks to buy, and the receipt email is unchanged."
        },
    })))
}

#[derive(Deserialize)]
pub struct BillingSettingsBody {
    /// The operator's own words. Absent keeps the stored value; an empty string CLEARS it (the
    /// feature goes back to off).
    pub payment_instructions: Option<String>,
}

/// PUT /api/v1/admin/billing-settings
pub async fn update_billing_settings(
    State(state): State<AppState>,
    Extension(_claims): Extension<Claims>,
    Json(body): Json<BillingSettingsBody>,
) -> ApiResult<Json<Value>> {
    let Some(text) = body.payment_instructions else {
        return Err(AppError::BadRequest(
            "Send {\"payment_instructions\": \"...\"}. An empty string clears it.".to_string(),
        ));
    };
    billing_instructions::save(&state.pool, &text)
        .await
        .map_err(AppError::BadRequest)?;

    let saved = billing_instructions::load(&state.pool).await;
    Ok(Json(json!({
        "success": true,
        "payment_instructions": saved.clone().unwrap_or_default(),
        "payment_instructions_set": saved.is_some(),
    })))
}
