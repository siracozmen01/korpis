// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's identity (§7 of docs/02-architecture.md): an Ed25519 key the node generates for
//! itself, whose public half the control plane records at enrollment. Every session's first
//! message proves the node holds the private half by signing the string [`hello_message`] builds,
//! and the control plane checks it with [`verify`]. The agent and the control plane share this
//! one definition, so they cannot disagree about what is signed.

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use std::fmt::Write as _;

/// The length of a public key, raw.
pub const PUBLIC_KEY_LEN: usize = 32;

/// What `NodeAuth.signature` signs: `korpis.agent.v1.Hello|<node>|<seconds>.<nanos>|<nonce>`,
/// with the timestamp as its Unix seconds and its nanoseconds in exactly nine digits, and the
/// nonce in lowercase hexadecimal.
#[must_use]
pub fn hello_message(node: &str, seconds: i64, nanos: u32, nonce: &[u8]) -> Vec<u8> {
    let mut s = format!("korpis.agent.v1.Hello|{node}|{seconds}.{nanos:09}|");
    for b in nonce {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
    }
    s.into_bytes()
}

/// Whether `public` is a usable Ed25519 public key: 32 bytes naming a point on the curve.
#[must_use]
pub fn valid_public_key(public: &[u8]) -> bool {
    <[u8; PUBLIC_KEY_LEN]>::try_from(public)
        .is_ok_and(|bytes| VerifyingKey::from_bytes(&bytes).is_ok())
}

/// Whether `signature` is `public`'s signature over `message`. Strict verification: a signature
/// that verifies only under the lax rules some libraries accept is refused.
#[must_use]
pub fn verify(public: &[u8], message: &[u8], signature: &[u8]) -> bool {
    let Ok(bytes) = <[u8; PUBLIC_KEY_LEN]>::try_from(public) else { return false };
    let Ok(key) = VerifyingKey::from_bytes(&bytes) else { return false };
    let Ok(sig) = Signature::from_slice(signature) else { return false };
    key.verify_strict(message, &sig).is_ok()
}

/// The node's signature over `message`: what the agent sends.
#[must_use]
pub fn sign(key: &SigningKey, message: &[u8]) -> Vec<u8> {
    key.sign(message).to_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hello_verifies_only_under_its_own_key_and_exact_string() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let public = key.verifying_key().to_bytes();
        let message = hello_message("nd_1", 1_760_000_000, 5, &[0xab, 0x01]);
        assert_eq!(message, b"korpis.agent.v1.Hello|nd_1|1760000000.000000005|ab01".to_vec());
        let signature = sign(&key, &message);
        assert!(verify(&public, &message, &signature));
        let later = hello_message("nd_1", 1_760_000_001, 5, &[0xab, 0x01]);
        assert!(!verify(&public, &later, &signature), "the timestamp is signed");
        let other = SigningKey::from_bytes(&[4; 32]).verifying_key().to_bytes();
        assert!(!verify(&other, &message, &signature));
        assert!(valid_public_key(&public));
        assert!(!valid_public_key(&public[..31]));
    }
}
