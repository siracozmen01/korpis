// SPDX-License-Identifier: AGPL-3.0-or-later

//! Which recipes an organization runs (§7 of docs/09-recipes.md): a recipe is used when one of
//! the organization's accepted signers has signed its digest, or, by a deliberate and logged
//! decision, when nobody it accepts has. Signing proves who published a recipe, not whether they
//! should be trusted; whom to accept is the operator's (question 4 of §13 of
//! docs/17-security.md).
//!
//! The policy is evaluated against the digest before the recipe is fetched, and what vouched
//! for the recipe is recorded in every intent declared from it.

#[cfg(feature = "trust")]
pub mod cosign;

/// A key an organization accepts recipes signed by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    /// For humans, and what an intent records as having vouched; unique within a policy.
    pub name: String,
    /// The public key, PEM-encoded `PUBLIC KEY` (`SubjectPublicKeyInfo`), as `cosign
    /// generate-key-pair` writes it.
    pub public_key_pem: String,
    /// `sha256:` of the key's DER encoding: its identity, whatever it is called.
    pub fingerprint: String,
}

/// One cosign signature as a registry holds it beside a recipe, and as the store keeps it: a
/// recipe's signatures are kept with it, so a rollback years later is verified without the
/// registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The simple-signing payload, exactly as signed.
    pub payload: Vec<u8>,
    /// The ASN.1 DER ECDSA signature over it, base64.
    pub signature: String,
}

/// An organization's recipe trust policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustPolicy {
    /// Signers whose signature makes a recipe usable.
    pub accepted_signers: Vec<Signer>,
    /// Use recipes none of `accepted_signers` has signed; every such use is logged.
    pub allow_unsigned: bool,
    /// Use imported recipes graded `unverified` (§9 of docs/09-recipes.md).
    pub allow_unverified: bool,
}

/// What vouched for a recipe, recorded in every intent declared from it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Vouched {
    /// An accepted signer's signature over the digest, by the signer's name.
    Signer(String),
    /// Nothing the organization accepts: used under `allow_unsigned`.
    Unsigned,
}

/// Why a recipe may not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Untrusted {
    /// No accepted signer signed it, and unsigned recipes are not allowed.
    #[error("no signer this organization accepts has signed {digest}")]
    NotSigned {
        /// The recipe's digest.
        digest: String,
    },
}

impl TrustPolicy {
    /// Decides a recipe whose digest the signers with `fingerprints` have validly signed.
    /// Signatures by keys the policy does not name prove nothing to it, so a recipe signed only
    /// by strangers is unsigned as far as this organization is concerned.
    pub fn decide(&self, digest: &str, fingerprints: &[String]) -> Result<Vouched, Untrusted> {
        if let Some(s) =
            self.accepted_signers.iter().find(|s| fingerprints.contains(&s.fingerprint))
        {
            return Ok(Vouched::Signer(s.name.clone()));
        }
        if self.allow_unsigned {
            return Ok(Vouched::Unsigned);
        }
        Err(Untrusted::NotSigned { digest: digest.to_owned() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signer(name: &str, fp: &str) -> Signer {
        Signer { name: name.into(), public_key_pem: String::new(), fingerprint: fp.into() }
    }

    #[test]
    fn an_accepted_signature_vouches_and_nothing_else_does_unless_allowed() {
        let mut p = TrustPolicy {
            accepted_signers: vec![signer("korpis", "sha256:k")],
            ..Default::default()
        };
        assert_eq!(p.decide("d", &["sha256:k".into()]), Ok(Vouched::Signer("korpis".into())));
        assert!(p.decide("d", &["sha256:stranger".into()]).is_err(), "a stranger proves nothing");
        assert!(p.decide("d", &[]).is_err());
        p.allow_unsigned = true;
        assert_eq!(p.decide("d", &[]), Ok(Vouched::Unsigned));
        assert_eq!(p.decide("d", &["sha256:k".into()]), Ok(Vouched::Signer("korpis".into())));
    }

    #[test]
    fn the_default_runs_nothing_until_the_operator_decides() {
        assert!(TrustPolicy::default().decide("d", &[]).is_err());
    }
}
