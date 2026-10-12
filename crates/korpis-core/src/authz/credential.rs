// SPDX-License-Identifier: AGPL-3.0-or-later

//! API tokens: the credential of a Token principal (§3.2 of docs/01-model.md).
//!
//! A secret is 256 bits from the operating system's generator with a fixed prefix, shown once
//! and stored only as its SHA-256 digest. With that much entropy a slow hash protects nothing,
//! and a fast one lets authentication be one indexed lookup.

use base64::Engine;
use sha2::{Digest, Sha256};
use std::fmt;
use std::time::SystemTime;
use uuid::Uuid;

/// Every secret starts with this, so a leaked one is recognizable to secret scanners.
pub const SECRET_PREFIX: &str = "korpis_";

/// A token's secret. Its `Debug` form is redacted so it never reaches a log by accident.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// The operating system could not supply randomness.
#[derive(Debug, Clone, thiserror::Error)]
#[error("no randomness available: {0}")]
pub struct EntropyError(String);

impl Secret {
    /// A fresh secret.
    pub fn generate() -> Result<Self, EntropyError> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| EntropyError(e.to_string()))?;
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        Ok(Self(format!("{SECRET_PREFIX}{body}")))
    }

    /// A secret as presented by a client. `None` if it cannot be one of ours.
    #[must_use]
    pub fn presented(s: &str) -> Option<Self> {
        s.starts_with(SECRET_PREFIX).then(|| Self(s.to_owned()))
    }

    /// The secret itself, for showing once.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The digest the store keeps.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        Sha256::digest(self.0.as_bytes()).into()
    }
}

/// A stored token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiToken {
    /// The token principal's identity.
    pub id: Uuid,
    /// For humans.
    pub name: String,
    /// When it stops working, if ever.
    pub expires_at: Option<SystemTime>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_prefixed_distinct_and_never_debug_printed() {
        let (a, b) = (Secret::generate().unwrap(), Secret::generate().unwrap());
        assert_ne!(a, b);
        assert!(a.expose().starts_with(SECRET_PREFIX));
        assert_eq!(format!("{a:?}"), "Secret(<redacted>)");
        assert_eq!(Secret::presented(a.expose()).map(|s| s.digest()), Some(a.digest()));
        assert!(Secret::presented("ghp_something").is_none());
    }
}
