//! "How to pay" — the operator's own payment details, for an install with no card provider.
//!
//! `payment_providers` holds ZERO rows on this installation and `POST /api/v1/checkout/create`
//! refuses every paid plan with `payment_provider_not_configured` (503), so a plan REQUEST is the
//! buying path: the customer presses a paid plan, a `tickets` row is filed, the operator is alerted
//! and the customer's receipt says "a person will reply". What was missing is the last step — a
//! customer who is ready to send money had nowhere to send it, because nothing in the product ever
//! told them how this business takes payment (a bank transfer, an invoice, another rail).
//!
//! The operator writes that once, in his own words, on the Payment Providers screen. The same
//! words then reach the customer on two surfaces:
//!
//!   * `GET /api/v1/checkout/status` carries them, so the Plan & Billing screen shows them beside
//!     the plan ladder — including after a reload;
//!   * the receipt mail for a filed plan request prints them as "How to pay now:".
//!
//! Absent or empty stays exactly as before: nothing is shown and the mail is unchanged. That is the
//! feature switch, and it is the operator's — this module never invents payment details.
//!
//! Stored in `admin_settings` key `billing` as `{"payment_instructions": "..."}`. The writer MERGES
//! that field into the row (jsonb `||`) so a future billing setting on the same key survives a save
//! from this screen.
//!
//! Fleet parity: FunnelSwift and IncentiveSwift shipped this on 2026-11-16 and ADASwift proved it
//! live the same day; this is the MissedCall side.

use sqlx::PgPool;

/// The longest blob the console may store. Generous enough for bank details plus a paragraph of
/// guidance, small enough that it cannot be used as a message board.
pub const MAX_LEN: usize = 4000;

/// The operator's saved words, or `None` when the feature is off (absent row, absent field, or an
/// empty/whitespace string). Callers treat `None` as "say nothing new".
pub async fn load(pool: &PgPool) -> Option<String> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT value->>'payment_instructions' FROM admin_settings WHERE key = 'billing'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Save the operator's words. An empty string is a deliberate CLEAR (the feature goes back to off),
/// never a silent no-op.
pub async fn save(pool: &PgPool, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.chars().count() > MAX_LEN {
        return Err(format!(
            "The payment instructions are too long ({} characters). Keep them under {}.",
            text.chars().count(),
            MAX_LEN
        ));
    }
    sqlx::query(
        "INSERT INTO admin_settings (key, value, description, updated_at)
         VALUES ('billing',
                 jsonb_build_object('payment_instructions', $1::text),
                 'Platform billing settings (admin-editable): how a customer pays while card payments are off',
                 NOW())
         ON CONFLICT (key) DO UPDATE
            SET value = admin_settings.value || jsonb_build_object('payment_instructions', $1::text),
                updated_at = NOW()",
    )
    .bind(text)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to save the payment instructions: {e}"))?;
    Ok(())
}
