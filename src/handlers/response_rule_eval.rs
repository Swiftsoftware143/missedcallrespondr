//! The Response Rules evaluator (kanban t_31f9cf38).
//!
//! The `response_rules` table was CRUD-only: `GET/POST/PUT/DELETE /api/v1/response-rules` stored a
//! rule, the plan counter `max_rules` counted it, and NO inbound-call path ever read one. Meanwhile
//! both consoles offered the screen and both guides said the rule "is evaluated on every inbound
//! call" — a surface promising an action with nothing behind it.
//!
//! This module is the missing half. On an inbound call [`run_for_inbound_call`] walks the called
//! tenant's ACTIVE rules in `priority` order (lowest first, `created_at` breaking ties), fires the
//! FIRST rule whose trigger matches, performs its action, and STOPS — exactly the order the guide has
//! always documented.
//!
//! ## Trigger vocabulary (`response_rules.trigger_condition`)
//!
//! | token | fires when | parameters (`response_rules.schedule`) |
//! |---|---|---|
//! | `all_missed_calls` | always (this app records every inbound ring as a missed call) | — |
//! | `specific_numbers` | the caller is one of the listed numbers | `{"numbers":["+1…", …]}` |
//! | `time_of_day` | the call arrives inside the window (a window may cross midnight) | `{"window":{"start":"HH:MM","end":"HH:MM"}}` |
//! | `day_of_week` | the call arrives on one of the listed days (UTC, the clock the store uses) | `{"days":["mon", …]}` |
//!
//! An unknown token never fires; [`validate_rule`] refuses to store one, so a rule can only be
//! created through a surface that would leave it dead if it could not be read here.
//!
//! ## Actions (`response_type`)
//!
//! * `sms` — texts the caller from the number they dialed, through the SAME transport the console's
//!   Send Message form uses ([`crate::handlers::message_handler::send_rule_sms`]). The row in the
//!   Messages log carries the PROVIDER'S OWN answer, never ours.
//! * `callback` — queues a return call: a `follow_ups` row (`follow_type='call_back'`, due in an
//!   hour, pending) — the same record the console's Follow Ups screen lists and `POST
//!   /api/v1/calls/:id/respond` writes. The queue is worked by a person; nothing dials by itself.
//!
//! `email` and `voice` were offered by the consoles and are gone: there is no recipient address for
//! a caller (a call carries no email), and this service has no outbound-call transport at all, so
//! neither could ever have done anything. [`validate_rule`] refuses them instead of storing a rule
//! that can never fire usefully.

use chrono::{Datelike, Duration, NaiveDateTime, Timelike};
use serde_json::Value;
use uuid::Uuid;

use crate::error::AppError;
use crate::models::response_rule::ResponseRule;
use crate::state::AppState;

/// Every trigger this service can evaluate. Stored verbatim in `response_rules.trigger_condition`.
pub const TRIGGERS: [&str; 4] = [
    "all_missed_calls",
    "specific_numbers",
    "time_of_day",
    "day_of_week",
];

/// Every action a rule may take. Stored verbatim in `response_rules.response_type`.
pub const RESPONSE_TYPES: [&str; 2] = ["sms", "callback"];

const WEEKDAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

fn quoted(values: &[&str]) -> String {
    values
        .iter()
        .map(|v| format!("\"{}\"", v))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Refuse a rule whose stored shape could never do anything, naming the field and what is accepted.
///
/// This runs on create and update — the two writers — so the store cannot hold a rule the evaluator
/// would silently skip (an unknown trigger, a response type with no transport behind it, or an `sms`
/// rule with no text to send, which is what the console used to save: `response_content:{text:''}`).
pub fn validate_rule(
    trigger_condition: &str,
    response_type: &str,
    response_content: &Value,
    schedule: Option<&Value>,
) -> Result<(), AppError> {
    let trigger = trigger_condition.trim();
    if !TRIGGERS.contains(&trigger) {
        return Err(AppError::BadRequest(format!(
            "trigger_condition must be one of {} (got \"{}\")",
            quoted(&TRIGGERS),
            trigger_condition
        )));
    }

    let kind = response_type.trim();
    if !RESPONSE_TYPES.contains(&kind) {
        return Err(AppError::BadRequest(format!(
            "response_type must be one of {} (got \"{}\"): this service can text the caller back or \
             queue a callback, and nothing else",
            quoted(&RESPONSE_TYPES),
            response_type
        )));
    }

    if kind == "sms" && rule_text(response_content).trim().is_empty() {
        return Err(AppError::BadRequest(
            "response_content.text is required for an \"sms\" rule: enter the message to send"
                .to_string(),
        ));
    }

    match trigger {
        "specific_numbers" if listed_numbers(schedule).is_empty() => {
            return Err(AppError::BadRequest(
                "schedule.numbers is required for a \"specific_numbers\" rule: list at least \
                 one caller number"
                    .to_string(),
            ));
        }
        "time_of_day" if window(schedule).is_none() => {
            return Err(AppError::BadRequest(
                "schedule.window.start and schedule.window.end (\"HH:MM\") are required for a \
                 \"time_of_day\" rule"
                    .to_string(),
            ));
        }
        "day_of_week" if listed_days(schedule).is_empty() => {
            return Err(AppError::BadRequest(format!(
                "schedule.days is required for a \"day_of_week\" rule: list at least one of {}",
                quoted(&WEEKDAYS)
            )));
        }
        _ => {}
    }

    Ok(())
}

/// The text a rule would send, when it is the `sms` action.
pub fn rule_text(response_content: &Value) -> String {
    response_content
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn listed_numbers(schedule: Option<&Value>) -> Vec<String> {
    schedule
        .and_then(|s| s.get("numbers"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn listed_days(schedule: Option<&Value>) -> Vec<String> {
    schedule
        .and_then(|s| s.get("days"))
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|d| d.trim().to_ascii_lowercase())
                .filter(|d| WEEKDAYS.contains(&d.as_str()))
                .collect()
        })
        .unwrap_or_default()
}

/// `("HH:MM" start, minutes-from-midnight start, end)` when the window is well formed.
fn window(schedule: Option<&Value>) -> Option<(String, u32, u32)> {
    let w = schedule.and_then(|s| s.get("window"))?;
    let start = w.get("start").and_then(|v| v.as_str())?.trim();
    let end = w.get("end").and_then(|v| v.as_str())?.trim();
    Some((start.to_string(), parse_hhmm(start)?, parse_hhmm(end)?))
}

fn parse_hhmm(value: &str) -> Option<u32> {
    let (h, m) = value.split_once(':')?;
    let h: u32 = h.trim().parse().ok()?;
    let m: u32 = m.trim().parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(h * 60 + m)
}

/// Digits only, so formatting ("(555) 010-1234" vs "+15550101234") is never a match reason.
fn digits(value: &str) -> String {
    value.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Two phone numbers describe the same line when one's digits END WITH the other's.
///
/// Formatting, separators and the country code are not a reason to miss a caller: operators copy
/// numbers from their own call log (`+15550101234`) as often as they type them locally
/// (`(555) 010-1234`). Both sides must carry at least 7 digits, so an empty or garbage value can
/// never match — the same floor the manual send's sender check uses.
fn same_number(left: &str, right: &str) -> bool {
    let a = digits(left);
    let b = digits(right);
    a.len() >= 7 && b.len() >= 7 && (a == b || a.ends_with(&b) || b.ends_with(&a))
}

/// Does this rule fire for this call? Pure, so it is unit-testable without a database.
pub fn trigger_matches(rule: &ResponseRule, caller: &str, now: NaiveDateTime) -> bool {
    match rule.trigger_condition.trim() {
        // This service records every inbound ring as a missed call (the webhook answers the call and
        // captures the caller as a lead), so "all missed calls" is every inbound call. The old
        // parenthetical here claimed a voicemail recorded "on the tenant's behalf" — retired with the
        // voicemail surface (kanban t_1d4fc956): no recording is requested and none is ever stored.
        "all_missed_calls" => true,
        "specific_numbers" => listed_numbers(rule.schedule.as_ref())
            .iter()
            .any(|n| same_number(caller, n)),
        "time_of_day" => match window(rule.schedule.as_ref()) {
            Some((_, start, end)) => {
                let minutes = now.hour() * 60 + now.minute();
                if start <= end {
                    minutes >= start && minutes <= end
                } else {
                    // a window that crosses midnight, e.g. 18:00 -> 08:00
                    minutes >= start || minutes <= end
                }
            }
            None => false,
        },
        "day_of_week" => {
            let token = WEEKDAYS[now.date().weekday().num_days_from_monday() as usize];
            listed_days(rule.schedule.as_ref())
                .iter()
                .any(|d| d == token)
        }
        // Only reachable for a row written before validation existed; it never fires rather than
        // firing on a guess.
        _ => false,
    }
}

/// The whole evaluator: pick the first matching ACTIVE rule and perform its action.
///
/// Best-effort by design — it is spawned from the inbound webhook so a slow provider call can never
/// delay the call-control answer Telnyx is waiting for, and it reports what happened as a row
/// (a `messages` row for `sms`, a `follow_ups` row for `callback`) plus a log line.
pub async fn run_for_inbound_call(
    state: &AppState,
    tenant_id: Uuid,
    call_id: Uuid,
    caller: &str,
    called: &str,
) {
    let rules: Vec<ResponseRule> = match sqlx::query_as::<_, ResponseRule>(
        "SELECT * FROM response_rules
         WHERE tenant_id = $1 AND is_active = true
         ORDER BY priority ASC, created_at ASC",
    )
    .bind(tenant_id)
    .fetch_all(&state.pool)
    .await
    {
        Ok(rules) => rules,
        Err(e) => {
            tracing::error!(
                "response rules: could not read tenant {}'s rules for call {}: {}",
                tenant_id,
                call_id,
                e
            );
            return;
        }
    };

    let now = chrono::Utc::now().naive_utc();
    for rule in &rules {
        if !trigger_matches(rule, caller, now) {
            continue;
        }
        tracing::info!(
            "response rule \"{}\" ({}) fires for call {} from {} to {}",
            rule.name,
            rule.trigger_condition,
            call_id,
            caller,
            called
        );
        perform(state, tenant_id, call_id, rule, caller, called, now).await;
        // The first matching rule fires and its action is executed; lower-priority rules are
        // skipped for that call (the documented order).
        return;
    }

    if !rules.is_empty() {
        tracing::debug!(
            "response rules: {} active rule(s) for tenant {}, none matched call {}",
            rules.len(),
            tenant_id,
            call_id
        );
    }
}

/// Do what the rule says — and record it.
async fn perform(
    state: &AppState,
    tenant_id: Uuid,
    call_id: Uuid,
    rule: &ResponseRule,
    caller: &str,
    called: &str,
    now: NaiveDateTime,
) {
    match rule.response_type.trim() {
        "sms" => {
            let text = rule_text(&rule.response_content);
            if text.trim().is_empty() {
                tracing::warn!(
                    "response rule \"{}\" has no message text (call {}); nothing was sent",
                    rule.name,
                    call_id
                );
                return;
            }
            // The sender is the number the caller dialed: that is the tenant's own number, and it is
            // what resolved the tenant in the first place.
            match crate::handlers::message_handler::send_rule_sms(
                state, tenant_id, call_id, called, caller, &text, &rule.name,
            )
            .await
            {
                Ok(id) => tracing::info!(
                    "response rule \"{}\": text queued with the provider for call {} (message {})",
                    rule.name,
                    call_id,
                    id
                ),
                // A missing credential is the operator's gap, not the caller's: it is logged and
                // (by the transport's contract) writes no row, because nothing was attempted.
                Err(e) => tracing::error!(
                    "response rule \"{}\": the automatic text for call {} was NOT sent: {:?}",
                    rule.name,
                    call_id,
                    e
                ),
            }
        }
        "callback" => {
            let notes = format!(
                "Response rule \"{}\" queued this callback for {}",
                rule.name, caller
            );
            let inserted = sqlx::query(
                "INSERT INTO follow_ups (id, call_id, follow_type, scheduled_at, status, notes, tenant_id, created_at, updated_at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            )
            .bind(Uuid::new_v4())
            .bind(call_id)
            .bind("call_back")
            .bind(now + Duration::hours(1))
            .bind("pending")
            .bind(&notes)
            .bind(tenant_id)
            .bind(now)
            .bind(now)
            .execute(&state.pool)
            .await;

            match inserted {
                Ok(_) => tracing::info!(
                    "response rule \"{}\": a callback is queued for call {} (follow-up due in an hour)",
                    rule.name,
                    call_id
                ),
                Err(e) => tracing::error!(
                    "response rule \"{}\": could not queue the callback for call {}: {}",
                    rule.name,
                    call_id,
                    e
                ),
            }
        }
        other => tracing::warn!(
            "response rule \"{}\" has an unsupported response_type \"{}\" and was skipped (call {})",
            rule.name,
            other,
            call_id
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use serde_json::json;

    fn rule(
        trigger: &str,
        kind: &str,
        content: serde_json::Value,
        schedule: Option<serde_json::Value>,
    ) -> ResponseRule {
        ResponseRule {
            id: Uuid::new_v4(),
            name: "test".into(),
            trigger_condition: trigger.into(),
            response_type: kind.into(),
            response_content: content,
            schedule,
            tenant_id: Uuid::new_v4(),
            is_active: true,
            priority: 100,
            created_at: NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
            updated_at: NaiveDate::from_ymd_opt(2026, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        }
    }

    fn at(h: u32, m: u32) -> NaiveDateTime {
        // 2026-10-02 is a Friday.
        NaiveDate::from_ymd_opt(2026, 10, 2)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    #[test]
    fn all_missed_calls_fires_on_every_call() {
        let r = rule("all_missed_calls", "sms", json!({"text": "hi"}), None);
        assert!(trigger_matches(&r, "+15550100111", at(3, 0)));
        assert!(trigger_matches(&r, "+15550100111", at(23, 59)));
    }

    #[test]
    fn specific_numbers_matches_on_digits_not_formatting() {
        let r = rule(
            "specific_numbers",
            "sms",
            json!({"text": "hi"}),
            Some(json!({"numbers": ["(555) 010-1234", "+15550109999"]})),
        );
        assert!(trigger_matches(&r, "+15550101234", at(12, 0)));
        assert!(trigger_matches(&r, "5550101234", at(12, 0)));
        assert!(!trigger_matches(&r, "+15550100000", at(12, 0)));
        // a short/garbage caller can never match
        assert!(!trigger_matches(&r, "1234", at(12, 0)));
    }

    #[test]
    fn time_of_day_window_and_the_midnight_crossing() {
        let day = rule(
            "time_of_day",
            "sms",
            json!({"text": "hi"}),
            Some(json!({"window": {"start": "09:00", "end": "17:00"}})),
        );
        assert!(trigger_matches(&day, "+15550100111", at(9, 0)));
        assert!(trigger_matches(&day, "+15550100111", at(17, 0)));
        assert!(!trigger_matches(&day, "+15550100111", at(8, 59)));
        assert!(!trigger_matches(&day, "+15550100111", at(17, 1)));

        let night = rule(
            "time_of_day",
            "sms",
            json!({"text": "hi"}),
            Some(json!({"window": {"start": "18:00", "end": "08:00"}})),
        );
        assert!(trigger_matches(&night, "+15550100111", at(23, 0)));
        assert!(trigger_matches(&night, "+15550100111", at(7, 59)));
        assert!(!trigger_matches(&night, "+15550100111", at(12, 0)));
    }

    #[test]
    fn day_of_week_matches_the_stored_token() {
        let r = rule(
            "day_of_week",
            "sms",
            json!({"text": "hi"}),
            Some(json!({"days": ["fri", "sat"]})),
        );
        assert!(trigger_matches(&r, "+15550100111", at(12, 0))); // Friday
        let monday = rule(
            "day_of_week",
            "sms",
            json!({"text": "hi"}),
            Some(json!({"days": ["mon"]})),
        );
        assert!(!trigger_matches(&monday, "+15550100111", at(12, 0)));
    }

    #[test]
    fn an_unknown_trigger_never_fires() {
        let r = rule("banana", "sms", json!({"text": "hi"}), None);
        assert!(!trigger_matches(&r, "+15550100111", at(12, 0)));
    }

    fn refused(
        trigger: &str,
        kind: &str,
        content: &serde_json::Value,
        schedule: Option<&serde_json::Value>,
    ) -> String {
        let e = validate_rule(trigger, kind, content, schedule).unwrap_err();
        format!("{:?}", e)
    }

    #[test]
    fn a_rule_shape_that_could_never_work_is_refused() {
        let text = json!({"text": "hi"});
        assert!(validate_rule("all_missed_calls", "sms", &text, None).is_ok());
        assert!(validate_rule("all_missed_calls", "callback", &json!({}), None).is_ok());

        // An empty message body is the shape the consoles used to save: nothing to send.
        assert!(
            refused("all_missed_calls", "sms", &json!({"text": "  "}), None)
                .contains("response_content.text")
        );
        assert!(refused("banana", "sms", &text, None).contains("trigger_condition"));
        for dead in ["email", "voice"] {
            assert!(refused("all_missed_calls", dead, &text, None).contains("response_type"));
        }

        let no_numbers = json!({"numbers": []});
        assert!(refused("specific_numbers", "sms", &text, Some(&no_numbers))
            .contains("schedule.numbers"));

        let bad_window = json!({"window": {"start": "9am"}});
        assert!(refused("time_of_day", "sms", &text, Some(&bad_window)).contains("schedule.window"));

        let bad_days = json!({"days": ["funday"]});
        assert!(refused("day_of_week", "sms", &text, Some(&bad_days)).contains("schedule.days"));
    }
}
