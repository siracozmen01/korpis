// SPDX-License-Identifier: AGPL-3.0-or-later

//! The issuer's signing keys (RFC 7517), as ring verifies with them: RSA for RS256, P-256 for
//! ES256, and Ed25519 for EdDSA. A key published for encryption, of another kind or curve, or
//! naming an algorithm other than its kind's, is left out rather than guessed at.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{self, RsaPublicKeyComponents, UnparsedPublicKey};
use serde_json::Value;

/// A signature algorithm an ID token may be signed with. `none` and every HMAC are not among
/// them: a token a client could have made itself proves nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alg {
    /// RSASSA-PKCS1-v1_5 with SHA-256.
    Rs256,
    /// ECDSA on P-256 with SHA-256.
    Es256,
    /// Ed25519.
    EdDsa,
}

impl Alg {
    /// The algorithm a JOSE header names, if it is one of these.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "RS256" => Some(Self::Rs256),
            "ES256" => Some(Self::Es256),
            "EdDSA" => Some(Self::EdDsa),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
enum Material {
    Rsa {
        n: Vec<u8>,
        e: Vec<u8>,
    },
    /// An uncompressed P-256 point, or an Ed25519 public key.
    Point(Vec<u8>),
}

/// One signing key.
#[derive(Debug, Clone)]
pub struct Key {
    /// Its identifier, if the issuer gave it one.
    pub kid: Option<String>,
    /// The one algorithm it verifies.
    pub alg: Alg,
    material: Material,
}

impl Key {
    /// Whether `sig` is its signature by `alg` over `message`.
    #[must_use]
    pub fn verifies(&self, alg: Alg, message: &[u8], sig: &[u8]) -> bool {
        if alg != self.alg {
            return false;
        }
        match (&self.material, alg) {
            (Material::Rsa { n, e }, Alg::Rs256) => RsaPublicKeyComponents { n, e }
                .verify(&signature::RSA_PKCS1_2048_8192_SHA256, message, sig)
                .is_ok(),
            (Material::Point(p), Alg::Es256) => {
                UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, p)
                    .verify(message, sig)
                    .is_ok()
            }
            (Material::Point(p), Alg::EdDsa) => {
                UnparsedPublicKey::new(&signature::ED25519, p).verify(message, sig).is_ok()
            }
            _ => false,
        }
    }
}

fn bytes(jwk: &Value, field: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(jwk.get(field)?.as_str()?).ok()
}

/// One key of a set, if it is a signing key of a kind verified here.
fn key(jwk: &Value) -> Option<Key> {
    if jwk.get("use").is_some_and(|u| u.as_str() != Some("sig")) {
        return None;
    }
    let text = |f: &str| jwk.get(f).and_then(Value::as_str);
    let (alg, material) = match (text("kty")?, text("crv")) {
        ("RSA", _) => (Alg::Rs256, Material::Rsa { n: bytes(jwk, "n")?, e: bytes(jwk, "e")? }),
        ("EC", Some("P-256")) => {
            let (x, y) = (bytes(jwk, "x")?, bytes(jwk, "y")?);
            if x.len() != 32 || y.len() != 32 {
                return None;
            }
            (Alg::Es256, Material::Point([&[4][..], &x, &y].concat()))
        }
        ("OKP", Some("Ed25519")) => (Alg::EdDsa, Material::Point(bytes(jwk, "x")?)),
        _ => return None,
    };
    if text("alg").is_some_and(|named| Alg::named(named) != Some(alg)) {
        return None;
    }
    Some(Key { kid: text("kid").map(str::to_owned), alg, material })
}

/// The signing keys of a JWK set; an empty list when it holds none.
#[must_use]
pub fn keys(set: &Value) -> Vec<Key> {
    set.get("keys")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |k| k.iter().filter_map(key).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_signing_keys_of_a_kind_verified_here_are_kept() {
        let x = URL_SAFE_NO_PAD.encode([7_u8; 32]);
        let set = json!({ "keys": [
            { "kty": "OKP", "crv": "Ed25519", "x": x, "kid": "a" },
            { "kty": "OKP", "crv": "Ed25519", "x": x, "use": "enc" },
            { "kty": "oct", "k": x },
            { "kty": "EC", "crv": "P-384", "x": x, "y": x },
            { "kty": "EC", "crv": "P-256", "x": x, "y": x, "alg": "RS256" },
            { "kty": "RSA", "n": x, "e": "AQAB" },
        ] });
        let kept = keys(&set);
        let algs: Vec<Alg> = kept.iter().map(|k| k.alg).collect();
        assert_eq!(algs, [Alg::EdDsa, Alg::Rs256]);
        assert_eq!(kept.first().and_then(|k| k.kid.as_deref()), Some("a"));
        assert_eq!(Alg::named("HS256"), None);
        assert_eq!(Alg::named("none"), None);
    }
}
