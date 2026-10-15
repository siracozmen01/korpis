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

/// Every API token secret starts with this, so a leaked one is recognizable to secret scanners.
pub const SECRET_PREFIX: &str = "korpis_";

/// Every workload credential starts with this (§9.5 of docs/08-identity.md): distinct from an
/// API token's, so authentication knows which record to look for and a scanner which kind it
/// found.
pub const WORKLOAD_PREFIX: &str = "korpiswl_";

/// Which kind of credential a secret is, by its prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    /// An API token's: the credential of a Token subject.
    ApiToken,
    /// A running workload's, issued through its node (§9.5 of docs/08-identity.md).
    Workload,
}

impl SecretKind {
    const fn prefix(self) -> &'static str {
        match self {
            Self::ApiToken => SECRET_PREFIX,
            Self::Workload => WORKLOAD_PREFIX,
        }
    }
}

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
pub struct EntropyError(pub(crate) String);

impl Secret {
    /// A fresh API token secret.
    pub fn generate() -> Result<Self, EntropyError> {
        Self::generate_kind(SecretKind::ApiToken)
    }

    /// A fresh secret of `kind`.
    pub fn generate_kind(kind: SecretKind) -> Result<Self, EntropyError> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| EntropyError(e.to_string()))?;
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        Ok(Self(format!("{}{body}", kind.prefix())))
    }

    /// An API token secret as presented by a client. `None` if it cannot be one of ours.
    #[must_use]
    pub fn presented(s: &str) -> Option<Self> {
        s.starts_with(SECRET_PREFIX).then(|| Self(s.to_owned()))
    }

    /// A secret of either kind as presented by a client, with its kind.
    #[must_use]
    pub fn presented_any(s: &str) -> Option<(SecretKind, Self)> {
        [SecretKind::Workload, SecretKind::ApiToken]
            .into_iter()
            .find(|k| s.starts_with(k.prefix()))
            .map(|k| (k, Self(s.to_owned())))
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

/// How long a workload credential lasts (§9.5 of docs/08-identity.md).
pub const WORKLOAD_CREDENTIAL_LIFETIME: std::time::Duration = std::time::Duration::from_secs(900);

/// A workload credential as stored: who it authenticates as, and the placement it was issued
/// under, which it is checked against at every use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkloadCredential {
    /// The workload it was issued for.
    pub workload: Uuid,
    /// Who it authenticates as: the workload, or the extension the workload runs.
    pub subject: crate::principal::Principal,
    /// The node that asked for it.
    pub node: Uuid,
    /// The lease epoch the node held the workload under.
    pub epoch: u64,
    /// When it was issued.
    pub issued_at: SystemTime,
    /// When it stops working.
    pub expires_at: SystemTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_prefixed_distinct_and_never_debug_printed() {
        let (a, b) = (Secret::generate().unwrap(), Secret::generate().unwrap());
        assert_ne!(a, b);
        assert!(a.expose().starts_with(SECRET_PREFIX));
        let w = Secret::generate_kind(SecretKind::Workload).unwrap();
        assert_eq!(Secret::presented_any(w.expose()).map(|(k, _)| k), Some(SecretKind::Workload));
        assert_eq!(Secret::presented_any(a.expose()).map(|(k, _)| k), Some(SecretKind::ApiToken));
        assert!(Secret::presented(w.expose()).is_none(), "an API token is never a workload's");
        assert_eq!(format!("{a:?}"), "Secret(<redacted>)");
        assert_eq!(Secret::presented(a.expose()).map(|s| s.digest()), Some(a.digest()));
        assert!(Secret::presented("ghp_something").is_none());
    }
}
