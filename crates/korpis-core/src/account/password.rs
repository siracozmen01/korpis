// SPDX-License-Identifier: AGPL-3.0-or-later

//! Passwords (§9.6 of docs/08-identity.md): Argon2id with the parameters OWASP recommends,
//! 19 MiB, two passes, one lane, stored as the PHC string so the parameters travel with the hash
//! and a later raise of them rehashes on the next sign-in. A password is never a sign-in alone.

use argon2::password_hash::{PasswordHash, PasswordHasher as _, PasswordVerifier as _, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

/// The shortest password accepted, in characters.
pub const MIN_LENGTH: usize = 12;
/// The longest, in bytes: a bound on what one sign-in can make the control plane hash.
pub const MAX_BYTES: usize = 1024;

/// Why a password cannot be set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PasswordError {
    /// Shorter than [`MIN_LENGTH`] characters.
    #[error("a password has at least {MIN_LENGTH} characters")]
    TooShort,
    /// Longer than [`MAX_BYTES`] bytes.
    #[error("a password has at most {MAX_BYTES} bytes")]
    TooLong,
    /// Hashing failed: randomness, or the parameters.
    #[error("the password could not be hashed: {0}")]
    Hash(String),
}

fn hasher() -> Result<Argon2<'static>, PasswordError> {
    let params =
        Params::new(19 * 1024, 2, 1, None).map_err(|e| PasswordError::Hash(e.to_string()))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// The stored form of `password`, a PHC string, if it may be one.
pub fn hash(password: &str) -> Result<String, PasswordError> {
    if password.chars().count() < MIN_LENGTH {
        return Err(PasswordError::TooShort);
    }
    if password.len() > MAX_BYTES {
        return Err(PasswordError::TooLong);
    }
    let salt = super::random::<16>().map_err(|e| PasswordError::Hash(e.0))?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| PasswordError::Hash(e.to_string()))?;
    hasher()?
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| PasswordError::Hash(e.to_string()))
}

/// Whether `password` is the one `stored` was made from. A stored form that does not parse, or
/// a password longer than any accepted, is simply not a match.
#[must_use]
pub fn matches(password: &str, stored: &str) -> bool {
    if password.len() > MAX_BYTES {
        return false;
    }
    let Ok(parsed) = PasswordHash::new(stored) else { return false };
    hasher().is_ok_and(|h| h.verify_password(password.as_bytes(), &parsed).is_ok())
}

/// A hash, with today's parameters, of a random password nobody kept: what a sign-in naming no
/// account checks its password against, so it takes as long as one naming an account does.
pub const STAND_IN: &str = "$argon2id$v=19$m=19456,t=2,p=1$9iggPYZ1U00DFX67MGD4Zg$8fKl0z+QH5D/Mude9t4AbjDnJWqk/w8C1T+jUVc19j4";

/// Whether `stored` was made with parameters weaker than today's, so the next successful
/// sign-in should store a new hash.
#[must_use]
pub fn outdated(stored: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored) else { return true };
    let current = Params::new(19 * 1024, 2, 1, None).ok();
    let theirs = Params::try_from(&parsed).ok();
    parsed.algorithm.as_str() != "argon2id"
        || match (current, theirs) {
            (Some(c), Some(t)) => t.m_cost() < c.m_cost() || t.t_cost() < c.t_cost(),
            _ => true,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_is_hashed_with_argon2id_and_matches_only_itself() {
        let stored = hash("correct horse battery").unwrap();
        assert!(stored.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{stored}");
        assert!(matches("correct horse battery", &stored));
        assert!(!matches("correct horse batterz", &stored));
        assert!(!matches("anything", "not a hash"));
        assert!(!outdated(&stored));
        assert_eq!(hash("too short"), Err(PasswordError::TooShort));
        assert_eq!(hash(&"x".repeat(MAX_BYTES + 1)), Err(PasswordError::TooLong));
    }

    #[test]
    fn the_stand_in_is_checked_as_long_as_a_real_hash() {
        assert!(PasswordHash::new(STAND_IN).is_ok());
        assert!(!outdated(STAND_IN), "raise it with the parameters");
        assert!(!matches("correct horse battery", STAND_IN));
    }
}
