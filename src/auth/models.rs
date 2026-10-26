use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};

pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2.hash_password(password.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

/// Check a password against a STORED hash.
///
/// A stored value that is not a usable PHC string is not a credential: no password can ever match
/// it, so it answers `Ok(false)` — the ordinary "wrong password" answer — instead of an `Err` the
/// caller turns into a 500. Measured live 2026-10-26 on this app: a `users` row whose
/// `password_hash` was a non-PHC placeholder made `PUT /api/v1/auth/password` answer
/// `500 {"error":"password hash string missing field"}` — announcing that the row is broken and
/// handing the caller the hasher's internal message, where the honest answer is the same
/// `401 "Current password is incorrect"` a mistyped password gets. Same rule the fleet states
/// elsewhere (CoreSwift-CRM 0bb4d05, FunnelSwift f2e6198, WorkflowSwift's login).
pub fn verify_password(password: &str, hash: &str) -> Result<bool, argon2::password_hash::Error> {
    let Ok(parsed_hash) = PasswordHash::new(hash) else {
        return Ok(false);
    };
    let argon2 = Argon2::default();
    Ok(argon2
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok())
}

use crate::config::Claims;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};

pub fn create_token(claims: &Claims, secret: &str) -> Result<String, jsonwebtoken::errors::Error> {
    encode(
        &Header::default(),
        claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

pub fn validate_token(token: &str, secret: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
    let token_data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(token_data.claims)
}
