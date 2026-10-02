//! Shared client-string length validation (kanban t_dd7be032).
//!
//! THE CLASS. A client-supplied string is bound to a bounded column (`VARCHAR(n)`) with no length
//! check, so a client-side typo reaches the driver, PostgreSQL refuses the statement, and the caller
//! gets `500 {"error":"Database error"}` where a `400` naming the field belongs. Measured live
//! 2026-10-02: with a 300-character `name`, `POST /api/v1/leads` and `POST /api/v1/clients` both
//! answered 500 and wrote nothing. The same class was fixed on `POST /api/v1/telnyx/numbers`
//! (kanban t_c30d5d52), which is where [`check_len`] came from; it now lives here because more than
//! two handlers need it.
//!
//! WHY A 400 AND NOT A TRUNCATION. Silently truncating to the column width would store a name the
//! caller never typed and never tell them. A bounded column is a limit on the FIELD, and a request
//! that breaks it is the caller's error (`error.rs` documents 400 as this app's bad-request shape).
//!
//! WHY CHARACTERS. PostgreSQL counts CHARACTERS for `VARCHAR(n)` — `length()` is the constraint,
//! `octet_length()` is not — so a 255-character multi-byte name is legal and must not be refused
//! early. `str::chars().count()` matches the column.
//!
//! WHERE TO CHECK. Before any statement (including a plan/feature gate that reads the database) and
//! before any provider call, so a bad request costs nothing.

use crate::error::AppError;

/// Refuse a client-supplied string longer than the column it is bound to with a 400 naming the
/// field and the limit.
pub fn check_len(field: &str, value: &str, max_chars: usize) -> Result<(), AppError> {
    let len = value.chars().count();
    if len > max_chars {
        return Err(AppError::BadRequest(format!(
            "{} is too long: {} characters, the maximum is {}",
            field, len, max_chars
        )));
    }
    Ok(())
}

/// [`check_len`] for an optional field: `None` binds nothing and passes; `Some(v)` is checked.
pub fn check_opt_len(field: &str, value: Option<&str>, max_chars: usize) -> Result<(), AppError> {
    match value {
        Some(v) => check_len(field, v, max_chars),
        None => Ok(()),
    }
}

/// Column bounds, each taken from `information_schema.columns.character_maximum_length` for the
/// column the value is bound to (`select table_name||'.'||column_name||' '||
/// character_maximum_length ...`). One constant per (table, column): the census that produced these
/// is the census that decided every check, so a schema change is a one-line change here.
pub mod max {
    // leads
    pub const LEADS_NAME: usize = 255;
    pub const LEADS_PHONE: usize = 32;
    pub const LEADS_EMAIL: usize = 320;
    pub const LEADS_SOURCE: usize = 64;
    pub const LEADS_STATUS: usize = 32;

    // clients
    pub const CLIENTS_NAME: usize = 255;
    pub const CLIENTS_EMAIL: usize = 320;
    pub const CLIENTS_PHONE: usize = 32;
    pub const CLIENTS_SOURCE: usize = 64;

    // lists
    pub const LISTS_NAME: usize = 255;

    // tags / tag_groups
    pub const TAGS_NAME: usize = 255;
    pub const TAGS_COLOR: usize = 7;
    pub const TAG_GROUPS_NAME: usize = 255;
    pub const TAG_GROUPS_COLOR: usize = 7;

    // campaigns
    pub const CAMPAIGNS_NAME: usize = 255;
    pub const CAMPAIGNS_KIND: usize = 32;
    pub const CAMPAIGNS_STATUS: usize = 32;

    // deals
    pub const DEALS_NAME: usize = 255;
    pub const DEALS_STAGE: usize = 64;
    pub const DEALS_SOURCE: usize = 64;

    // tickets / ticket_messages
    pub const TICKETS_PRIORITY: usize = 16;
    pub const TICKETS_SOURCE: usize = 32;
    pub const TICKETS_STATUS: usize = 32;
    pub const TICKET_MESSAGES_SENDER_TYPE: usize = 16;

    // workflows / workflow_steps
    pub const WORKFLOWS_NAME: usize = 255;
    pub const WORKFLOWS_TRIGGER_EVENT: usize = 64;
    pub const WORKFLOW_STEPS_ACTION_TYPE: usize = 32;

    // calendar_events
    pub const CALENDAR_EVENTS_TITLE: usize = 255;
    pub const CALENDAR_EVENTS_EVENT_TYPE: usize = 64;

    // export_templates
    pub const EXPORT_TEMPLATES_NAME: usize = 255;
    pub const EXPORT_TEMPLATES_ENTITY: usize = 64;
    pub const EXPORT_TEMPLATES_FORMAT: usize = 16;

    // integration_targets
    pub const INTEGRATION_TARGETS_NAME: usize = 255;
    pub const INTEGRATION_TARGETS_PROVIDER: usize = 100;

    // api_keys / provider_keys
    pub const API_KEYS_NAME: usize = 255;
    /// `api_keys.prefix` — the SERVER-generated key prefix. The census found this column answering
    /// 500 for every create (see `api_key_handler::API_KEY_PREFIX`), so its bound is pinned here too.
    pub const API_KEYS_PREFIX: usize = 8;
    pub const PROVIDER_KEYS_PROVIDER: usize = 64;
    pub const PROVIDER_KEYS_BASE_URL: usize = 512;
    pub const PROVIDER_KEYS_SCOPE: usize = 16;

    // portfolio_companies
    pub const PORTFOLIO_COMPANIES_NAME: usize = 255;
    pub const PORTFOLIO_COMPANIES_SLUG: usize = 255;

    // payment_providers
    pub const PAYMENT_PROVIDERS_LABEL: usize = 255;
    // `payment_providers.provider_type` is VARCHAR(32) but is never a free string: the handler
    // refuses anything outside {stripe, paypal, square, paddle}, so no length constant is needed.
    pub const PAYMENT_PROVIDERS_PUBLISHABLE_KEY: usize = 255;

    // tenant_plans
    pub const TENANT_PLANS_BILLING_CYCLE: usize = 50;

    // import_logs
    pub const IMPORT_LOGS_ENTITY: usize = 64;
    pub const IMPORT_LOGS_FILENAME: usize = 512;
    pub const IMPORT_LOGS_STATUS: usize = 32;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(field: &str, value: &str, max: usize) -> Option<String> {
        match check_len(field, value, max) {
            Err(AppError::BadRequest(msg)) => Some(msg),
            _ => None,
        }
    }

    /// The boundary every live probe measures: the limit itself is accepted, one character more is a
    /// 400 naming the field and the limit — never a silent truncation, never a 500 from the driver.
    #[test]
    fn the_limit_is_accepted_and_one_over_is_a_400_that_names_the_field() {
        assert!(check_len("name", &"y".repeat(255), max::LEADS_NAME).is_ok());
        let msg = refusal("name", &"z".repeat(256), max::LEADS_NAME).expect("256 must be refused");
        assert_eq!(msg, "name is too long: 256 characters, the maximum is 255");
        assert!(
            msg.contains("name") && msg.contains("255"),
            "the message must name the field and the limit: {msg}"
        );
    }

    /// `VARCHAR(n)` counts CHARACTERS, not bytes — a multi-byte name of legal length must pass, and
    /// the count in the message is a character count.
    #[test]
    fn the_check_counts_characters_not_bytes() {
        assert!(check_len("title", &"é".repeat(255), max::CALENDAR_EVENTS_TITLE).is_ok());
        // 300 chars, 600 bytes: refused for the character count, not the byte count.
        let msg = refusal("title", &"é".repeat(300), max::CALENDAR_EVENTS_TITLE)
            .expect("300 chars must be refused");
        assert_eq!(msg, "title is too long: 300 characters, the maximum is 255");
    }

    #[test]
    fn an_absent_optional_field_is_skipped_and_a_present_one_is_checked() {
        assert!(check_opt_len("phone", None, max::LEADS_PHONE).is_ok());
        assert!(check_opt_len("phone", Some(""), max::LEADS_PHONE).is_ok());
        assert!(check_opt_len("phone", Some("+15550000001"), max::LEADS_PHONE).is_ok());
        assert!(check_opt_len("phone", Some(&"5".repeat(33)), max::LEADS_PHONE).is_err());
    }

    /// The narrow `/^(#[0-9a-f]{6})$/i`-shaped colour column is 7 characters; anything longer is the
    /// caller's error. Pinned because 7 is the smallest bound in the app.
    #[test]
    fn the_smallest_bound_in_the_app_is_seven() {
        assert!(check_len("color", "#6366f1", max::TAGS_COLOR).is_ok());
        let msg = refusal("color", "#6366f1a", max::TAGS_COLOR).expect("8 must be refused");
        assert_eq!(msg, "color is too long: 8 characters, the maximum is 7");
    }
}
