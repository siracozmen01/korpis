// SPDX-License-Identifier: AGPL-3.0-or-later

//! A credential's public key, as COSE encodes it (RFC 9052, RFC 9053), and the three signature
//! algorithms platform authenticators produce: `ES256`, `EdDSA` over `Ed25519`, and `RS256`, which
//! Windows Hello still uses. Any other algorithm, curve, or key size is refused at registration,
//! so an assertion is never asked to verify one.

use super::WebAuthnError;
use coset::cbor::value::Value;
use coset::{CborSerializable as _, CoseKey, KeyType, Label, RegisteredLabelWithPrivate, iana};
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};

/// The signature algorithm of a credential's key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// ECDSA over P-256 with SHA-256 (COSE -7).
    Es256,
    /// `EdDSA` over `Ed25519` (COSE -8).
    EdDsa,
    /// RSASSA-PKCS1-v1_5 with SHA-256 (COSE -257).
    Rs256,
}

impl Algorithm {
    /// Its COSE identifier.
    #[must_use]
    pub const fn cose(self) -> i64 {
        match self {
            Self::Es256 => -7,
            Self::EdDsa => -8,
            Self::Rs256 => -257,
        }
    }
}

/// A key, in the form its verifier takes.
enum PublicKey {
    /// An uncompressed P-256 point.
    Es256(Vec<u8>),
    /// An Ed25519 key.
    EdDsa(Vec<u8>),
    /// An RSA modulus and exponent.
    Rs256 { n: Vec<u8>, e: Vec<u8> },
}

/// The COSE identifiers of the curves accepted (RFC 9053, table 18).
const P_256: i128 = 1;
const ED25519: i128 = 6;

fn refused(what: &'static str) -> WebAuthnError {
    WebAuthnError::Key(what)
}

/// The algorithm of the COSE key `cose`, if it is one accepted.
pub(super) fn algorithm(cose: &[u8]) -> Result<Algorithm, WebAuthnError> {
    Ok(match decode(cose)? {
        PublicKey::Es256(_) => Algorithm::Es256,
        PublicKey::EdDsa(_) => Algorithm::EdDsa,
        PublicKey::Rs256 { .. } => Algorithm::Rs256,
    })
}

/// Verifies `signature` over `message` under the COSE key `cose`.
pub(super) fn verify(cose: &[u8], message: &[u8], signature: &[u8]) -> Result<(), WebAuthnError> {
    let verified = match decode(cose)? {
        PublicKey::Es256(point) => {
            UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_ASN1, point)
                .verify(message, signature)
        }
        PublicKey::EdDsa(key) => {
            UnparsedPublicKey::new(&signature::ED25519, key).verify(message, signature)
        }
        PublicKey::Rs256 { n, e } => RsaPublicKeyComponents { n, e }.verify(
            &signature::RSA_PKCS1_2048_8192_SHA256,
            message,
            signature,
        ),
    };
    verified.map_err(|_| WebAuthnError::Signature)
}

fn decode(cose: &[u8]) -> Result<PublicKey, WebAuthnError> {
    let key = CoseKey::from_slice(cose).map_err(|_| refused("not a COSE key"))?;
    let Some(RegisteredLabelWithPrivate::Assigned(alg)) = key.alg else {
        return Err(refused("no algorithm"));
    };
    let param =
        |label: i64| key.params.iter().find_map(|(l, v)| (*l == Label::Int(label)).then_some(v));
    let bytes = |label: i64| {
        param(label).and_then(|v| v.as_bytes().cloned()).ok_or_else(|| refused("a parameter"))
    };
    let curve = |label: i64| param(label).and_then(Value::as_integer).map(i128::from);
    match (&key.kty, alg) {
        (KeyType::Assigned(iana::KeyType::EC2), iana::Algorithm::ES256) => {
            if curve(-1) != Some(P_256) {
                return Err(refused("an ES256 key not on P-256"));
            }
            let (x, y) = (bytes(-2)?, bytes(-3)?);
            if x.len() != 32 || y.len() != 32 {
                return Err(refused("a P-256 coordinate's length"));
            }
            let mut point = vec![4];
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            Ok(PublicKey::Es256(point))
        }
        (KeyType::Assigned(iana::KeyType::OKP), iana::Algorithm::EdDSA) => {
            if curve(-1) != Some(ED25519) {
                return Err(refused("an EdDSA key not on Ed25519"));
            }
            let x = bytes(-2)?;
            if x.len() != 32 {
                return Err(refused("an Ed25519 key's length"));
            }
            Ok(PublicKey::EdDsa(x))
        }
        (KeyType::Assigned(iana::KeyType::RSA), iana::Algorithm::RS256) => {
            let (n, e) = (bytes(-1)?, bytes(-2)?);
            if n.len() < 256 {
                return Err(refused("an RSA key shorter than 2048 bits"));
            }
            Ok(PublicKey::Rs256 { n, e })
        }
        _ => Err(refused("an algorithm not accepted")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An RSA-2048 modulus, its exponent 65537, and its PKCS #1 v1.5 SHA-256 signature over
    /// [`MESSAGE`], made with `openssl dgst -sha256 -sign` on 2026-10-16.
    const MODULUS: [&str; 6] = [
        "a7395c6de56f6ef769f8a76555da8858bfb131cc0db7c56417b79c70a2f4e075e22a770620018b3d4ebc70e8",
        "cf443fb9b6ff10e7e0a46b1aafd06bbe090d96d832d013865ff25e1e861227d1ea1c9ad04c9130a53087a251",
        "027724064a7ec6ba2c6d7ca691143986638379c9c0beac1f02d02364ae3a818af697dfe43b7f97fdf9c7b88b",
        "94ce2b64978a873782236c0b019c51cf84ace5e1c9e368845bc6775853f4d9c9c9665d349f6b87cee51101bd",
        "570ad1e015e310d51d1fb327a25c894807e1971cf19907677d39ac67783cbc7da75b0a602e7ece648bde722f",
        "2f8ee371c3b61497bb76cf326d14cbf34433938b4a14f7f183ed1bdfde40755f5d6d4109",
    ];
    const SIGNATURE: [&str; 6] = [
        "56e0d74a6c919578deb13b0ee9030c1fdd1aaf3a092f894e2290e73d064dd2fef02b9b7fcb0a445926477d37",
        "610198613203ce136c7bf1cda8630d7f3d07886e3a4db78dc83152a8a4d861e10fe4003ac708ca209e0790a1",
        "69100a1d850c16c29b921ec2751c58ed412fb383b469849a99d67f7960679d9b8617b8d69f53ecf65b574f95",
        "421bea157c6bc51a3842b92266ad767b6255660eb831f2f01526e38730c51948b9869c240c1b00c8a6326a2b",
        "ca9f8b7bbec2fcbbd98846ba6d2990550782d4cefd32c88f022985af06e536f32679b379e2ccaba874e834bd",
        "b31aa74f63f0a2852a17a3e57fba6ff24b68cd8d29f33ae99ec3c39a6ab91c9d6c2ab3ac",
    ];
    const MESSAGE: &[u8] = b"korpis rs256 test message";

    fn unhex(parts: &[&str]) -> Vec<u8> {
        let s: String = parts.concat();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn rsa_key(n: Vec<u8>) -> Vec<u8> {
        let key = CoseKey {
            kty: KeyType::Assigned(iana::KeyType::RSA),
            alg: Some(RegisteredLabelWithPrivate::Assigned(iana::Algorithm::RS256)),
            params: vec![
                (Label::Int(-1), Value::Bytes(n)),
                (Label::Int(-2), Value::Bytes(vec![1, 0, 1])),
            ],
            ..CoseKey::default()
        };
        key.to_vec().unwrap()
    }

    #[test]
    fn an_rs256_key_verifies_what_it_signed_and_nothing_else() {
        let key = rsa_key(unhex(&MODULUS));
        assert_eq!(algorithm(&key), Ok(Algorithm::Rs256));
        let signature = unhex(&SIGNATURE);
        assert_eq!(verify(&key, MESSAGE, &signature), Ok(()));
        assert_eq!(verify(&key, b"another message", &signature), Err(WebAuthnError::Signature));
    }

    #[test]
    fn a_key_too_short_or_of_another_algorithm_is_refused() {
        assert!(algorithm(&rsa_key(vec![1; 128])).is_err(), "1024 bits");
        let hmac = CoseKey {
            kty: KeyType::Assigned(iana::KeyType::Symmetric),
            alg: Some(RegisteredLabelWithPrivate::Assigned(iana::Algorithm::HMAC_256_256)),
            ..CoseKey::default()
        };
        assert!(algorithm(&hmac.to_vec().unwrap()).is_err());
        assert!(algorithm(b"not cbor").is_err());
    }
}
