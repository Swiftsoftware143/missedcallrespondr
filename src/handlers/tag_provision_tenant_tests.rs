//! Regression armour for card t_c9669881: the tag-provision handler used to bind a HARDCODED
//! tenant uuid to every contact it auto-provisioned for FunnelSwift, and that uuid
//! (`883a2a82-c7e4-4abb-b6c2-da47c119caf1`) exists in no database — no migration, seed or operator
//! step ever created it. `contacts.tenant_id -> tenants(id)` therefore rejected every insert of a
//! NEW email and the endpoint answered
//!
//!     500 Database error: ... violates foreign key constraint "contacts_tenant_id_fkey"
//!
//! so FunnelSwift could not create a single contact here. The owner is now a SLUG resolved at
//! runtime (`resolve_provision_tenant`), which cannot drift out of existence the way an id can.
//!
//! These legs pin the two halves that are reachable without a database: the source of the handler
//! must not contain a quoted uuid literal again, and the slug must never degrade to an empty
//! string. The `pre_fix_*` legs run the same scanners over the exact pre-fix expression, so a
//! passing leg is evidence (the scanner really does flag the old code) rather than a tautology.
#![cfg(test)]

use super::swallow_tests::{capture, test_state};
use super::tag_provision_handler::{
    provision_tenant_slug, DEFAULT_TENANT_NAME, DEFAULT_TENANT_SLUG,
};

/// The handler under test, as the compiler sees it.
const HANDLER_SRC: &str = include_str!("tag_provision_handler.rs");

/// The expression the handler used to bind as `contacts.tenant_id` — the exact pre-fix shape.
const PRE_FIX_BIND: &str =
    r#"    .bind("883a2a82-c7e4-4abb-b6c2-da47c119caf1".parse::<Uuid>().unwrap())"#;

fn is_uuid_shape(candidate: &str) -> bool {
    let bytes = candidate.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, b) in bytes.iter().enumerate() {
        let dashes = [8, 13, 18, 23];
        if dashes.contains(&i) {
            if *b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// Every QUOTED uuid literal in the source, ignoring `//` comment lines so prose that names the
/// old id (like this file and the handler's doc comments) is not mistaken for code.
fn quoted_uuid_literals(src: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in src.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        for (idx, _) in line.match_indices('"') {
            let rest = &line[idx + 1..];
            if rest.len() >= 36 && is_uuid_shape(&rest[..36]) {
                found.push(rest[..36].to_string());
            }
        }
    }
    found
}

#[test]
fn handler_binds_no_hardcoded_tenant_uuid() {
    let found = quoted_uuid_literals(HANDLER_SRC);
    assert!(
        found.is_empty(),
        "tag_provision_handler must not carry a uuid literal again (found {found:?}); the owner is \
         resolved by slug via resolve_provision_tenant (card t_c9669881)"
    );
}

/// Control leg: the scanner DOES flag the pre-fix expression. Without this, the leg above would
/// pass even if the scanner were broken.
#[test]
fn pre_fix_bind_is_flagged_by_the_same_scanner() {
    assert_eq!(
        quoted_uuid_literals(PRE_FIX_BIND),
        vec!["883a2a82-c7e4-4abb-b6c2-da47c119caf1".to_string()],
        "the scanner must detect the exact expression this card removed"
    );
}

/// Resolve the owner slug for a given `TAG_PROVISION_TENANT_SLUG` value, through the same state
/// builder every other leg uses. `test_state()` builds a pool, which needs a Tokio context, hence
/// the `capture` harness.
fn slug_for(configured: &str) -> String {
    let (slug, _log) = capture(move || {
        let mut state = test_state();
        state.config.tag_provision_tenant_slug = configured.to_string();
        async move { provision_tenant_slug(&state.config).to_string() }
    });
    slug
}

#[test]
fn owner_slug_is_the_seeded_default_when_unset() {
    assert_eq!(slug_for(""), DEFAULT_TENANT_SLUG);
}

#[test]
fn owner_slug_never_degrades_to_empty_whitespace() {
    assert_eq!(
        slug_for("   \t "),
        DEFAULT_TENANT_SLUG,
        "a blank TAG_PROVISION_TENANT_SLUG must not turn into a query for slug ''"
    );
}

#[test]
fn owner_slug_is_used_verbatim_when_configured() {
    assert_eq!(slug_for("  mcr-leads  "), "mcr-leads");
}

/// The slug the handler resolves and the slug migration 000018 seeds must agree, otherwise a fresh
/// database seeds one owner and the handler creates a second one.
#[test]
fn handler_default_slug_matches_the_migration_that_seeds_it() {
    let migration = include_str!("../../migrations/000018_funnelswift_tenant.sql");
    assert!(
        migration.contains(&format!("'{DEFAULT_TENANT_SLUG}'")),
        "migrations/000018_funnelswift_tenant.sql must seed slug '{DEFAULT_TENANT_SLUG}'"
    );
    assert!(
        migration.contains(&format!("'{DEFAULT_TENANT_NAME}'")),
        "migrations/000018 must use the same tenant name the handler creates ('{DEFAULT_TENANT_NAME}')"
    );
}
