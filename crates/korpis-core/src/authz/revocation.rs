// SPDX-License-Identifier: AGPL-3.0-or-later

//! The revocation list (§6.3 of docs/08-identity.md): the grants a verifier refuses in any
//! token's chain, computed whole by the control plane and signed by the token key, so a relay or
//! an agent trusts it exactly as far as it trusts the tokens it checks, and a copy cached on disk
//! carries its own proof.
//!
//! A list replaces the one before it. An entry is a grant revoked, or suspended and not lifted,
//! within the horizon: the longest a token can outlive the moment its grant stopped, plus the
//! clock skew a verifier is allowed. A grant revoked before the horizon needs no entry, because
//! every token naming it has expired by every verifier's clock.

use super::token::{KeyRing, LONGEST_TOKEN_LIFETIME, SigningKey, TokenError};
use ed25519_dalek::{Signature, Signer as _, VerifyingKey};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// The domain the signature is bound to, so that no other message the token key signs can be
/// read as a list. Biscuit's own signed payloads begin with a NUL byte; this does not.
const DOMAIN: &str = "korpis.revocation.v1\n";

/// How far back a list reaches, for verifiers whose clocks may run `skew` behind the control
/// plane's.
#[must_use]
pub fn horizon(now: SystemTime, skew: Duration) -> SystemTime {
    now.checked_sub(LONGEST_TOKEN_LIFETIME + skew).unwrap_or(UNIX_EPOCH)
}

/// A list, signed or not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RevocationList {
    /// When the control plane computed it, in milliseconds since the Unix epoch.
    pub version: u64,
    /// The grants listed, sorted and distinct.
    pub grants: Vec<Uuid>,
}

/// A list with its signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedList {
    /// The list.
    pub list: RevocationList,
    /// The epoch of the token key that signed it.
    pub key_id: u32,
    /// Ed25519 over [`RevocationList::canonical`].
    pub signature: Vec<u8>,
}

impl RevocationList {
    /// The list computed at `at` from `grants`, which are sorted and deduplicated here.
    #[must_use]
    pub fn new(at: SystemTime, mut grants: Vec<Uuid>) -> Self {
        grants.sort_unstable();
        grants.dedup();
        let millis = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        Self { version: u64::try_from(millis).unwrap_or(u64::MAX), grants }
    }

    /// The bytes the signature covers: the domain, the version in decimal and a newline, then
    /// each grant hyphenated and lowercase, each followed by a newline.
    #[must_use]
    pub fn canonical(&self) -> Vec<u8> {
        let mut out = String::with_capacity(DOMAIN.len() + 21 + self.grants.len() * 37);
        out.push_str(DOMAIN);
        out.push_str(&self.version.to_string());
        out.push('\n');
        for g in &self.grants {
            out.push_str(&g.hyphenated().to_string());
            out.push('\n');
        }
        out.into_bytes()
    }

    /// Signs it with the token key.
    #[must_use]
    pub fn sign(self, key: &SigningKey) -> SignedList {
        let signer = ed25519_dalek::SigningKey::from_bytes(&key.seed());
        let signature = signer.sign(&self.canonical()).to_bytes().to_vec();
        SignedList { list: self, key_id: key.key_id, signature }
    }

    /// Whether a token chain names a listed grant; the first one if so.
    #[must_use]
    pub fn refuses(&self, chain: &[Uuid]) -> Option<Uuid> {
        chain.iter().find(|g| self.grants.binary_search(g).is_ok()).copied()
    }
}

impl SignedList {
    /// The list, if `keys` holds the key it names and the signature verifies, and its grants are
    /// in the canonical order a signer produces.
    pub fn verify(self, keys: &KeyRing) -> Result<RevocationList, TokenError> {
        let public = keys.public_bytes(self.key_id).ok_or_else(|| {
            TokenError::Key(format!("the revocation list names unknown key {}", self.key_id))
        })?;
        let key = <[u8; 32]>::try_from(public.as_slice())
            .ok()
            .and_then(|b| VerifyingKey::from_bytes(&b).ok())
            .ok_or_else(|| TokenError::Key("a token key is not an Ed25519 key".into()))?;
        let signature = Signature::from_slice(&self.signature).map_err(|_| {
            TokenError::Invalid("the revocation list's signature is malformed".into())
        })?;
        if !self.list.grants.windows(2).all(|w| matches!(w, [a, b] if a < b)) {
            return Err(TokenError::Invalid(
                "the revocation list is not in canonical order".into(),
            ));
        }
        key.verify_strict(&self.list.canonical(), &signature).map_err(|_| {
            TokenError::Invalid("the revocation list's signature does not verify".into())
        })?;
        Ok(self.list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(key: &SigningKey) -> KeyRing {
        let mut ring = KeyRing::default();
        ring.insert(key.key_id, &key.public_bytes()).unwrap();
        ring
    }

    #[test]
    fn a_signed_list_verifies_with_the_token_key_and_nothing_else() {
        let key = SigningKey::generate(1);
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let list = RevocationList::new(SystemTime::now(), vec![b, a, b]);
        assert_eq!(list.grants, [a, b], "sorted and distinct");
        let signed = list.clone().sign(&key);
        assert_eq!(signed.clone().verify(&ring(&key)), Ok(list.clone()));

        let other = SigningKey::generate(1);
        assert!(matches!(signed.clone().verify(&ring(&other)), Err(TokenError::Invalid(_))));
        assert!(matches!(signed.clone().verify(&KeyRing::default()), Err(TokenError::Key(_))));

        let mut shortened = signed.clone();
        shortened.list.grants.pop();
        assert!(shortened.verify(&ring(&key)).is_err(), "an entry cannot be dropped");
        let mut older = signed;
        older.list.version -= 1;
        assert!(older.verify(&ring(&key)).is_err(), "nor its version changed");
    }

    #[test]
    fn a_chain_is_refused_by_any_listed_link() {
        let (leaf, root, other) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let list = RevocationList::new(SystemTime::now(), vec![root]);
        assert_eq!(list.refuses(&[leaf, root]), Some(root));
        assert_eq!(list.refuses(&[leaf, other]), None);
    }

    #[test]
    fn the_horizon_covers_the_longest_token_and_the_skew() {
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        let skew = Duration::from_secs(300);
        assert_eq!(horizon(now, skew), now - LONGEST_TOKEN_LIFETIME - skew);
    }
}
