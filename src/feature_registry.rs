//! The app's FEATURE REGISTRY — the ONE place that lists every plan-gated key this app enforces.
//!
//! Why it exists (kanban t_dd2f7e32, David's directive "the top tier plan gets everything"):
//! the gates in `features.rs` were wired key-by-key from handler call sites, and the `plans`
//! rows were authored at different times, so the key a gate reads and the key the plan data
//! carries could drift apart with NO error anywhere. Two of the keys below were granted by **no
//! plan at all** — `has_calendar` (so the flag gate refused EVERY tier, Enterprise included) and
//! `bring_your_own_key` (its `feature_limits` table was empty, so the BYOK arm refused everyone).
//! Nothing in the source said so; only enumerating the registry against the live rows did.
//!
//! Three more keys arrived in kanban t_b578b169, where the plan data advertised limits no gate read:
//! `max_tags` (the `plans.max_tags` column — free 10 / pro 50 / pro-monthly 50 / enterprise -1) and
//! `max_phone_numbers` (`plans.features` — free 1 / pro-monthly 5) are now GATED, each on the one
//! route that ADDS the counted row (`POST /api/v1/tags`, `POST /api/v1/telnyx/numbers`).
//! `max_users` was RETIRED from the plan data instead: this app has no surface that adds a user to
//! an existing tenant — all three `INSERT INTO users` sites create a tenant's FIRST user, before any
//! plan is attached — so a cap of 1/5 advertised a seat control the product does not have. An
//! allowance nothing can reach is a setting, not a control; the raw JSON values were removed by
//! `000022_retire_max_users_plan_value.sql` rather than left advertised.
//!
//! Two vocabularies are deliberate and shared by the gate, the panel and this file:
//!
//! * **value**: `-1` = unlimited / granted · `0` = NOT available on this plan (refused) ·
//!   `N > 0` = a cap of N. For a `Boolean` key, non-zero = on, `0` = off.
//! * **absence**: a key with NO row in either store resolves per `unset_means()` — a limit
//!   (absence ⇒ allowed, an inert gate) differs from a boolean (absence ⇒ REFUSED). The catalogue
//!   reports the resolved value AND its source, so the panel can never show a different trade
//!   than the gate enforced.
//!
//! `mod tests` (below) refuses to compile a build where a gate is called with a key that is not
//! in this list, or where this list names a key no gate reads — the anti-drift guard.

/// What sort of value a key carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureKind {
    /// On/off capability (calendar, BYOK, white-label).
    Boolean,
    /// Metered allowance (`max_*`), compared against live usage.
    Limit,
}

/// WHERE the panel writes a key, and therefore where the gate reads it from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// A dedicated `plans` column — the plan's own default (e.g. `plans.max_leads`).
    Column(&'static str),
    /// A `feature_limits` row (one per plan × key) — the panel-managed grant.
    FeatureLimits,
}

/// One plan-gated key the app enforces.
#[derive(Debug, Clone, Copy)]
pub struct FeatureDef {
    pub key: &'static str,
    /// Human label used in denial messages and in the panel's catalogue.
    pub label: &'static str,
    pub kind: FeatureKind,
    /// Unit for a `Limit` key (leads, users, …); `None` for booleans.
    pub unit: Option<&'static str>,
    pub storage: Storage,
    /// The route/handler that enforces it — printed in the panel so a key can be traced to its gate.
    pub enforced_by: &'static str,
    /// `true` when the key is passed to a gate function in `features.rs`
    /// (`enforce_feature_limit` / `check_feature_limit` / `check_feature_flag`). `false` only for a
    /// grant a handler reads itself (`features::entitlement_for_plan`); the test below then requires
    /// the key literal to appear in the source.
    pub read_by_gate: bool,
}

impl FeatureDef {
    /// What ABSENCE means for this key — the panel shows it in the cell so an unset key is never
    /// mistaken for a deliberate denial (or vice versa).
    pub fn unset_means(&self) -> &'static str {
        match self.kind {
            FeatureKind::Limit => "unset = allowed (no limit configured)",
            FeatureKind::Boolean => "unset = refused (no plan grants it)",
        }
    }
}

/// The registry: every plan-gated key this app enforces, in the order the panel shows them.
pub const REGISTRY: &[FeatureDef] = &[
    FeatureDef {
        key: "max_leads",
        label: "Leads",
        kind: FeatureKind::Limit,
        unit: Some("leads"),
        storage: Storage::Column("max_leads"),
        enforced_by: "POST /api/v1/leads (leads_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_contacts",
        label: "Contacts",
        kind: FeatureKind::Limit,
        unit: Some("contacts"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/contacts (contact_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_calls",
        label: "Calls",
        kind: FeatureKind::Limit,
        unit: Some("calls"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/calls (call_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_messages",
        label: "Messages",
        kind: FeatureKind::Limit,
        unit: Some("messages"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/messages (message_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_rules",
        label: "Response rules",
        kind: FeatureKind::Limit,
        unit: Some("rules"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/response-rules (response_rule_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_deals",
        label: "Deals",
        kind: FeatureKind::Limit,
        unit: Some("deals"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/deals (deals_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_campaigns",
        label: "Campaigns",
        kind: FeatureKind::Limit,
        unit: Some("campaigns"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/campaigns (campaigns_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_tickets",
        label: "Tickets",
        kind: FeatureKind::Limit,
        unit: Some("tickets"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/tickets (tickets_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_follow_ups",
        label: "Follow Ups",
        kind: FeatureKind::Limit,
        unit: Some("follow-ups"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/follow-ups (follow_up_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_integrations",
        label: "Integrations",
        kind: FeatureKind::Limit,
        unit: Some("integrations"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/integrations (integration_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_integration_targets",
        label: "Integration targets",
        kind: FeatureKind::Limit,
        unit: Some("targets"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/integration-targets (integration_target_handler::create)",
        read_by_gate: true,
    },
    // kanban t_f06b1710 — `max_api_keys` was REMOVED from the registry with the deleted API-key
    // route group. The registry exists to stop silent drift in both directions, and its own test
    // (`every_registry_key_is_read_by_a_gate`) refused a `read_by_gate: true` entry whose only gate
    // call site no longer exists — which is exactly right: a catalogue row the admin panel shows for
    // a capability with no route is the lie this file prevents. The residual plan DATA it carded is
    // CLOSED (kanban t_ab963d11): `feature_limits`' `enterprise | max_api_keys` row and the
    // `api_keys` table are retired by migrations/000027_retire_api_keys.sql, `plans.features` was
    // measured to mention no api key on any plan, and `features::count_usage`'s api arm is gone. So
    // the console cannot render, grant, count or re-seed the capability — no source, no plan DATA.
    FeatureDef {
        key: "max_message_templates",
        label: "Message templates",
        kind: FeatureKind::Limit,
        unit: Some("templates"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/message-templates (message_template_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_portfolio_companys",
        label: "Portfolio companies",
        kind: FeatureKind::Limit,
        unit: Some("companies"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/portfolio-companies (portfolio_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_tags",
        label: "Tags",
        kind: FeatureKind::Limit,
        unit: Some("tags"),
        storage: Storage::Column("max_tags"),
        enforced_by: "POST /api/v1/tags (tags_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "max_phone_numbers",
        label: "Phone numbers",
        kind: FeatureKind::Limit,
        unit: Some("numbers"),
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/telnyx/numbers (telnyx_handler::purchase_number)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "has_calendar",
        label: "Calendar",
        kind: FeatureKind::Boolean,
        unit: None,
        storage: Storage::FeatureLimits,
        enforced_by: "POST /api/v1/calendar-events (calendar_events_handler::create)",
        read_by_gate: true,
    },
    FeatureDef {
        key: "bring_your_own_key",
        label: "Bring your own Telnyx key (BYOK)",
        kind: FeatureKind::Boolean,
        unit: None,
        storage: Storage::FeatureLimits,
        // Read through features::entitlement_for_plan() by the BYOK arm of the provider-key upsert
        // rather than by a gate function — hence read_by_gate = false.
        enforced_by:
            "POST /api/v1/provider-keys (provider_keys_handler::upsert_provider_key, telnyx)",
        read_by_gate: false,
    },
];

/// Look a key up. Every write path validates against this, so a plan row can never carry a key
/// the app does not understand (or a typo nobody would notice).
pub fn find(key: &str) -> Option<&'static FeatureDef> {
    REGISTRY.iter().find(|f| f.key == key)
}

/// The registry keys, in order.
pub fn keys() -> impl Iterator<Item = &'static str> {
    REGISTRY.iter().map(|f| f.key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).expect("read_dir").flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().map(|x| x == "rs").unwrap_or(false) {
                out.push(p);
            }
        }
    }

    /// Every key a gate is CALLED with, read out of the shipped source. Only `handlers/` is
    /// scanned (every gate call site lives there); the definitions and the wrapper in
    /// `features.rs` are excluded so their bodies cannot be mistaken for call sites.
    fn gate_called_keys() -> BTreeSet<String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("handlers");
        let mut files = Vec::new();
        rs_files(&root, &mut files);
        assert!(!files.is_empty(), "no handler sources found under {root:?}");
        let mut keys = BTreeSet::new();
        for f in files {
            let src = fs::read_to_string(&f).expect("read handler source");
            for name in [
                "enforce_feature_limit(",
                "check_feature_limit(",
                "check_feature_flag(",
            ] {
                let mut rest = src.as_str();
                while let Some(i) = rest.find(name) {
                    let after = &rest[i + name.len()..];
                    // Everything up to the call's closing paren: the key is the first token in
                    // `"..."` form. (No call site nests a paren before its key.)
                    let args = match after.find(')') {
                        Some(j) => &after[..j],
                        None => after,
                    };
                    if let Some(open) = args.find('"') {
                        if let Some(close) = args[open + 1..].find('"') {
                            let lit = &args[open + 1..open + 1 + close];
                            if !lit.is_empty()
                                && lit.len() < 40
                                && lit.chars().all(|c| {
                                    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'
                                })
                            {
                                keys.insert(lit.to_string());
                            }
                        }
                    }
                    rest = &rest[i + name.len()..];
                }
            }
        }
        keys
    }

    /// A gate called with a key the registry does not know is exactly the silent drift this file
    /// exists to stop (`plans.features` misses ⇒ the absence rule decides ⇒ nothing enforced).
    #[test]
    fn every_gate_call_site_is_in_the_registry() {
        let called = gate_called_keys();
        let unknown: Vec<&String> = called.iter().filter(|k| find(k).is_none()).collect();
        assert!(
            unknown.is_empty(),
            "gate(s) call keys that are NOT in the feature registry: {unknown:?} — add them to REGISTRY"
        );
        assert!(
            called.len() >= 15,
            "expected the gate call sites to be enumerated, found only {called:?}"
        );
    }

    /// The reverse: a registry entry nothing reads is a lie in the panel. A key read by
    /// hand-written SQL (`read_by_gate = false`) must at least appear in the source.
    #[test]
    fn every_registry_key_is_read_by_a_gate() {
        let called = gate_called_keys();
        for def in REGISTRY {
            if def.read_by_gate {
                assert!(
                    called.contains(def.key),
                    "registry key `{}` is read by no gate — remove it or wire the gate (enforced_by: {})",
                    def.key,
                    def.enforced_by
                );
            } else {
                let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
                let mut files = Vec::new();
                rs_files(&root, &mut files);
                let present = files.iter().any(|f| {
                    fs::read_to_string(f)
                        .map(|s| s.contains(def.key))
                        .unwrap_or(false)
                });
                assert!(
                    present,
                    "registry key `{}` says it is read outside the gate module, but the literal is nowhere in src/",
                    def.key
                );
            }
        }
    }
}
