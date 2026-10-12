// SPDX-License-Identifier: AGPL-3.0-or-later

//! cosign's keyed signatures, in the form `cosign sign --key` stores beside an artifact: a
//! manifest tagged `sha256-<hex>.sig` whose layers are simple-signing payloads naming the signed
//! digest, each with an ECDSA P-256 signature over the payload in an annotation.
//!
//! Only what the payload names is believed: a signature over a payload for another digest, or of
//! another type, verifies nothing.

use super::Signer;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use p256::ecdsa::signature::{Signer as _, Verifier as _};
use p256::ecdsa::{Signature as Sig, SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePublicKey, EncodePublicKey};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// The media type of a signature layer.
pub const SIMPLE_SIGNING_MEDIA_TYPE: &str = "application/vnd.dev.cosign.simplesigning.v1+json";
/// The layer annotation that carries the signature, base64.
pub const SIGNATURE_ANNOTATION: &str = "dev.cosignproject.cosign/signature";
/// The payload type cosign signs.
const SIGNATURE_TYPE: &str = "cosign container image signature";

/// One signature as the registry holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The simple-signing payload, exactly as signed.
    pub payload: Vec<u8>,
    /// The ASN.1 DER ECDSA signature over it, base64.
    pub signature: String,
}

/// Why a public key is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// Not a PEM `PUBLIC KEY` for ECDSA over P-256, the key `cosign generate-key-pair` makes.
    #[error("not a PEM ECDSA P-256 public key: {0}")]
    NotP256(String),
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::from("sha256:"), |mut s, b| {
        // Writing to a String cannot fail.
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// The tag cosign stores `digest`'s signatures under, or `None` for a malformed digest.
#[must_use]
pub fn signature_tag(digest: &str) -> Option<String> {
    crate::recipe::reference::is_digest(digest).then(|| digest.replacen(':', "-", 1) + ".sig")
}

/// A signer from its name and PEM public key, with the key's fingerprint.
pub fn signer(name: &str, pem: &str) -> Result<Signer, KeyError> {
    let key = VerifyingKey::from_public_key_pem(pem.trim())
        .map_err(|e| KeyError::NotP256(e.to_string()))?;
    let der = key.to_public_key_der().map_err(|e| KeyError::NotP256(e.to_string()))?;
    Ok(Signer {
        name: name.to_owned(),
        public_key_pem: pem.trim().to_owned(),
        fingerprint: hex_sha256(der.as_bytes()),
    })
}

/// The simple-signing payload naming `digest` in `repository`.
#[must_use]
pub fn payload(repository: &str, digest: &str) -> Vec<u8> {
    serde_json::json!({
        "critical": {
            "identity": {"docker-reference": repository},
            "image": {"docker-manifest-digest": digest},
            "type": SIGNATURE_TYPE,
        },
        "optional": null,
    })
    .to_string()
    .into_bytes()
}

/// Whether `payload` is a cosign signature payload naming exactly `digest`.
fn names(payload: &[u8], digest: &str) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(payload) else { return false };
    let critical = v.get("critical");
    let field = |path: &[&str]| path.iter().try_fold(critical?, |at, key| at.get(key));
    field(&["type"]).and_then(serde_json::Value::as_str) == Some(SIGNATURE_TYPE)
        && field(&["image", "docker-manifest-digest"]).and_then(serde_json::Value::as_str)
            == Some(digest)
}

fn verifies(key: &VerifyingKey, s: &Signature) -> bool {
    let Ok(der) = STANDARD.decode(s.signature.trim()) else { return false };
    let Ok(sig) = Sig::from_der(&der) else { return false };
    key.verify(&s.payload, &sig).is_ok()
}

/// The fingerprints of the `signers` that validly signed `digest` among `signatures`.
#[must_use]
pub fn verified(digest: &str, signatures: &[Signature], signers: &[Signer]) -> Vec<String> {
    let naming: Vec<&Signature> = signatures.iter().filter(|s| names(&s.payload, digest)).collect();
    signers
        .iter()
        .filter(|signer| {
            VerifyingKey::from_public_key_pem(&signer.public_key_pem)
                .is_ok_and(|key| naming.iter().any(|s| verifies(&key, s)))
        })
        .map(|signer| signer.fingerprint.clone())
        .collect()
}

/// Signs `payload` as cosign does: ECDSA P-256 over its SHA-256, ASN.1 DER, base64. For the
/// signing tooling of Phase 4 (§4 of docs/20-roadmap.md) and for tests.
#[must_use]
pub fn sign(key: &SigningKey, payload: &[u8]) -> String {
    let sig: Sig = key.sign(payload);
    STANDARD.encode(sig.to_der().as_bytes())
}

/// The PEM public key of a signing key.
pub fn public_pem(key: &SigningKey) -> Result<String, KeyError> {
    key.verifying_key()
        .to_public_key_pem(p256::pkcs8::LineEnding::LF)
        .map_err(|e| KeyError::NotP256(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f9d2f";

    fn key(n: u8) -> SigningKey {
        SigningKey::from_slice(&[n; 32]).unwrap()
    }

    fn signed(k: &SigningKey, digest: &str) -> Signature {
        let payload = payload("registry.example/paper", digest);
        Signature { signature: sign(k, &payload), payload }
    }

    #[test]
    fn a_signature_verifies_only_for_its_key_and_the_digest_it_names() {
        let (ours, theirs) = (key(1), key(2));
        let us = signer("us", &public_pem(&ours).unwrap()).unwrap();
        let them = signer("them", &public_pem(&theirs).unwrap()).unwrap();
        let sigs = [signed(&ours, DIGEST)];
        let expected = vec![us.fingerprint.clone()];
        assert_eq!(verified(DIGEST, &sigs, &[us.clone(), them]), expected);
        let other = "sha256:".to_owned() + &"0".repeat(64);
        assert!(verified(&other, &sigs, std::slice::from_ref(&us)).is_empty(), "another digest");
        let [original] = sigs;
        let mut tampered = original;
        tampered.payload.push(b' ');
        assert!(verified(DIGEST, &[tampered], &[us]).is_empty(), "the payload is what is signed");
    }

    #[test]
    fn keys_are_p256_and_their_fingerprint_is_their_identity() {
        let pem = public_pem(&key(3)).unwrap();
        let a = signer("a", &pem).unwrap();
        assert_eq!(a.fingerprint, signer("b", &format!("\n{pem}\n")).unwrap().fingerprint);
        assert!(signer("x", "-----BEGIN PUBLIC KEY-----\nAAAA\n-----END PUBLIC KEY-----").is_err());
        assert_eq!(signature_tag(DIGEST).unwrap(), DIGEST.replace(':', "-") + ".sig");
    }
}
