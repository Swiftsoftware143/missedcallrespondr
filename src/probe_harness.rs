//! `probe_harness` — attribution for harness-created workspaces (kanban t_4a744754).
//!
//! The public signup (`POST /api/v1/auth/register`) mints a `tenants` row named
//! "<name>'s Workspace", so a workspace minted by a test harness through the real signup door is
//! byte-indistinguishable from a customer's. Policy
//! (`/opt/swift/docs/fleet-probe-residue-policy-2026-09-28.md`, answer 3c) says attribution must be
//! DATA; this is the durable half of that, ported verbatim from the shape proven on FunnelSwift
//! (t_fc88ec2a) and ADASwift (t_ae307966).
//!
//! A harness that mints a workspace through the real signup route may mark it with the
//! `X-Swift-Harness` request HEADER. The marker is read from the header ONLY — never from the
//! request body and never from a query field. The body of a signup request is client-controlled
//! data that the flow writes into customer-visible columns (`tenants.name`), so a body field would
//! let a customer sign their own workspace up as fleet machinery; a header is not part of any
//! customer-facing form and is the same channel the rest of the fleet's tooling uses.
//!
//! Storage contract: a workspace created by a real customer is byte-identical to what it was
//! before this module existed, because the marker is `NULL` unless the header is present AND
//! well-formed.

use axum::http::HeaderMap;

/// The request header a harness uses to mark the workspace it is about to create.
pub const PROBE_HARNESS_HEADER: &str = "x-swift-harness";

/// `^[a-z0-9][a-z0-9._-]{2,63}$` — 3 to 64 characters, so the first char plus at most 63 more.
const MIN_LEN: usize = 3;
const MAX_LEN: usize = 64;

/// Read `X-Swift-Harness` off the request headers and return the marker to store, if any.
///
/// Trim, then lowercase, then accept ONLY a value matching `^[a-z0-9][a-z0-9._-]{2,63}$`.
/// Everything else returns `None`, and the workspace is created exactly as every workspace was
/// created before this column existed:
///
/// * no header at all — the ordinary customer signup;
/// * a value shorter than 3 or longer than 64 characters;
/// * a first character that is not `[a-z0-9]`, or any character outside `[a-z0-9._-]`
///   (this rejects SQL/HTML metacharacters, whitespace inside the value, and every non-ASCII
///   character, so `'\";DROP TABLE users;--` can never reach the column);
/// * a header that is not valid UTF-8 (`HeaderValue::to_str`).
///
/// The accepted value is stored verbatim after trim+lowercase and is never interpreted — the
/// caller binds it as a query parameter, so even a hostile value could not alter the statement.
pub fn from_headers(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(PROBE_HARNESS_HEADER)?.to_str().ok()?;
    normalise(raw)
}

/// The validation half of [`from_headers`], split out so it can be tested without building a
/// `HeaderMap`.
fn normalise(raw: &str) -> Option<String> {
    let marker = raw.trim().to_lowercase();
    if marker.len() < MIN_LEN || marker.len() > MAX_LEN {
        return None;
    }
    let mut chars = marker.chars();
    let first = chars.next()?;
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return None;
    }
    if !marker
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
    {
        return None;
    }
    Some(marker)
}

/// The marker to store for the workspace the SIGNUP route is about to mint (kanban `t_4a744754`
/// follow-up, measured 2026-10-17).
///
/// Two arms, in order:
///
/// 1. the `X-Swift-Harness` request header, exactly as [`from_headers`] reads it; then
/// 2. a SERVER-DERIVED marker when the recipient's own address is on a fleet harness / reserved
///    domain (`crate::security::probe_addr::harness_domain` — the same predicate the mail layer
///    uses to suppress those sends).
///
/// WHY THE SECOND ARM EXISTS. A sweep of all six app databases on 2026-10-17 found 35 probe
/// workspaces minted inside the previous hour, and NOT ONE carried this column: the harnesses
/// that mint them do not send the header, so the only detector left was guessing at the workspace
/// NAME — and a harness that names its workspace innocuously ("Credit Note Probe's Workspace",
/// "Len Probe") then survives every sweep until the name pattern happens to grow a matching word.
/// Attribution is supposed to be DATA, and this is the one data the request cannot fake: the
/// address decides the mail destination anyway, and every domain in `harness_domain` is
/// fleet-owned / non-routable, so a workspace created on one is fleet machinery by construction.
///
/// A real customer is untouched: their domain is not in that list, so the answer is `None` and the
/// row is written exactly as it was before this column existed. The marker cannot be used to
/// mislabel a real workspace either — it is derived from the recipient domain, never from a
/// client-supplied string, and a customer on a reserved domain could not receive the credentials
/// mail in the first place.
pub fn for_signup(headers: &HeaderMap, email: &str) -> Option<String> {
    if let Some(marker) = from_headers(headers) {
        return Some(marker);
    }
    crate::security::probe_addr::harness_domain(email).map(|domain| format!("auto-{domain}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn a_signup_on_a_fleet_harness_domain_is_attributed_without_a_header() {
        // Measured defect 2026-10-17: probe workspaces minted through this route carried no marker
        // at all, so the only detector was guessing at their NAME.
        let none = HeaderMap::new();
        assert_eq!(
            for_signup(&none, "probe-tagproof1792235151@example.invalid").as_deref(),
            Some("auto-example.invalid")
        );
        assert_eq!(
            for_signup(&none, "mcrsweep1791551633@swiftsoftware.dev").as_deref(),
            Some("auto-swiftsoftware.dev")
        );
        // a subdomain classifies like its parent, because harness_domain() does
        assert_eq!(
            for_signup(&none, "x@mail.swiftsoftware.net").as_deref(),
            Some("auto-swiftsoftware.net")
        );
    }

    #[test]
    fn a_real_customer_address_is_still_null() {
        // The whole point of the second arm: an ordinary domain writes the row byte-identically.
        let none = HeaderMap::new();
        assert_eq!(for_signup(&none, "customer@gmail.com"), None);
        assert_eq!(for_signup(&none, "owner@their-business.com"), None);
        assert_eq!(for_signup(&none, "not an address"), None);
        // a domain that merely CONTAINS the harness name is not the harness class
        assert_eq!(for_signup(&none, "x@notswiftsoftware.dev"), None);
    }

    #[test]
    fn the_header_wins_over_the_address_arm() {
        // A harness that names itself keeps its own attribution, even from a harness domain.
        assert_eq!(
            for_signup(&headers_with("verify-x"), "probe@example.invalid").as_deref(),
            Some("verify-x")
        );
    }

    fn headers_with(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(PROBE_HARNESS_HEADER, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn a_well_formed_marker_is_stored_trimmed_and_lowercased() {
        assert_eq!(
            normalise("  Verify-T_4a744754  ").as_deref(),
            Some("verify-t_4a744754")
        );
        assert_eq!(normalise("abc").as_deref(), Some("abc"));
        assert_eq!(normalise("a.b_c-d9").as_deref(), Some("a.b_c-d9"));
        assert_eq!(
            normalise(&"a".repeat(64)).as_deref(),
            Some("a".repeat(64).as_str())
        );
    }

    #[test]
    fn everything_else_is_null() {
        // missing header -> None (not an error)
        assert_eq!(from_headers(&HeaderMap::new()), None);
        // too short / too long
        assert_eq!(normalise("ab"), None);
        assert_eq!(normalise(&"a".repeat(65)), None);
        // first char must be [a-z0-9]
        assert_eq!(normalise("-abc"), None);
        assert_eq!(normalise(".abc"), None);
        assert_eq!(normalise("_abc"), None);
        // illegal characters, incl. the SQL-injection probe from the card
        assert_eq!(normalise("'\";DROP TABLE users;--"), None);
        assert_eq!(normalise("verify t_4a744754"), None);
        assert_eq!(normalise("verify/../x"), None);
        assert_eq!(normalise("VERIFY"), Some("verify".to_string())); // lowercase is allowed
        assert_eq!(normalise("café-1"), None); // non-ASCII is rejected
                                               // empty / whitespace-only
        assert_eq!(normalise(""), None);
        assert_eq!(normalise("   "), None);
        // a well-formed value that only differs by case still round-trips
        assert_eq!(
            from_headers(&headers_with("Verify-T_4a744754")).as_deref(),
            Some("verify-t_4a744754")
        );
    }
}
