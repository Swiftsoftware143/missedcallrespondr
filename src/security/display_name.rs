//! Display-name validation — ONE rule, at every door that writes a person's name.
//!
//! Before this module the app had TWO answers to "how long may a name be": the profile screen
//! capped it at 200 characters (`update_profile`), and the public signup did not cap it at all.
//! `users.name`, `tenants.name` and `users.username` are `text` on this app, so nothing downstream
//! refused an over-long value — it was simply STORED, and the signup derives the workspace label
//! from the typed name (`"<name>'s Workspace"`). Measured live 2026-10-17 against a reserved
//! `.invalid` probe address:
//!
//!   POST /api/v1/auth/register  {"name":"<300 chars>",  "email":"…"}  ->  200, account created
//!   POST /api/v1/auth/register  {"name":"<4000 chars>", "email":"…"}  ->  200, account created
//!
//! …so a pasted paragraph became the customer's name, their workspace label, the greeting in the
//! welcome mail, and the account header — with no error at any point, because a `text` column has
//! no length to violate. The four sibling apps each refuse this at the door (their name columns are
//! `varchar(255)` and a shared guard answers 422 in words); this app is the only one that accepts it.
//!
//! The cap is NOT a new product decision: it is the limit this app's own profile screen already
//! enforces. Signup and profile must agree, or a name accepted at signup could never be re-saved
//! from the profile without shortening it. Trimming lives here too — the signup previously stored
//! `"   Ada   "` verbatim.

/// Longest display name this app accepts. Matches `update_profile`'s existing `MAX_FIELD`, so the
/// signup and profile doors cannot disagree.
pub const MAX_NAME_CHARS: usize = 200;

/// Normalise a person's display name for STORAGE, or explain why it cannot be stored.
///
/// Trims, requires a non-empty value, refuses one longer than [`MAX_NAME_CHARS`], and refuses
/// control characters (the name is rendered into HTML and into mail subjects). Returns the value
/// to persist — the caller must bind THIS, never the raw input.
pub fn normalize(raw: &str) -> Result<String, String> {
    normalize_capped(raw, MAX_NAME_CHARS, "name")
}

/// The same guard for a caller that owns its own cap and label.
pub fn normalize_capped(raw: &str, max_chars: usize, label: &str) -> Result<String, String> {
    let value = raw.trim();

    if value.is_empty() {
        return Err(format!("{label}: is required"));
    }
    if value.chars().count() > max_chars {
        return Err(format!("{label}: is longer than {max_chars} characters"));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(format!("{label}: must not contain control characters"));
    }

    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_the_lengths_the_signup_used_to_store() {
        // Measured live 2026-10-17: 300 and 4000 characters both answered 200 and created an
        // account. Both must now be a caller-safe 422 reason.
        for n in [MAX_NAME_CHARS + 1, 300, 4000] {
            assert_eq!(
                normalize(&"n".repeat(n)),
                Err(format!("name: is longer than {MAX_NAME_CHARS} characters"))
            );
        }
        // …and one at the cap still passes, so the guard does not shrink the contract.
        assert!(normalize(&"n".repeat(MAX_NAME_CHARS)).is_ok());
    }

    #[test]
    fn accepts_ordinary_and_unicode_names() {
        assert_eq!(normalize("  Ada Lovelace  ").unwrap(), "Ada Lovelace");
        assert_eq!(normalize("Ó Súilleabháin").unwrap(), "Ó Súilleabháin");
        // 150 accented characters are 150 characters — the cap counts characters, not bytes.
        assert!(normalize(&"é".repeat(150)).is_ok());
    }

    #[test]
    fn rejects_empty_and_control_characters() {
        assert_eq!(normalize("   "), Err("name: is required".to_string()));
        assert_eq!(normalize(""), Err("name: is required".to_string()));
        assert!(normalize("Ada\nLovelace").is_err());
    }

    #[test]
    fn the_derived_workspace_label_fits_after_the_cap() {
        // The signup appends "'s Workspace" to the stored name; the label stays a sane length.
        let name = normalize(&"n".repeat(MAX_NAME_CHARS)).unwrap();
        assert!(format!("{name}'s Workspace").chars().count() <= MAX_NAME_CHARS + 12);
    }
}
