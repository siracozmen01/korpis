// SPDX-License-Identifier: AGPL-3.0-or-later

//! A compact JWS (RFC 7515), the form an ID token takes, read and verified against the issuer's
//! keys. A token naming a key identifier is verified by that key alone; one naming none, by any
//! key of its algorithm. Nothing here is believed before its signature is.

use crate::jwk::{Alg, Key};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

/// The most an ID token may be, in bytes: far beyond what issuers make, and a bound on what a
/// response can make the provider parse.
pub const MAX_TOKEN: usize = 64 << 10;

/// A token read, not yet verified.
#[derive(Debug, Clone)]
pub struct Signed {
    /// Its algorithm.
    pub alg: Alg,
    /// The key it names, if it names one.
    pub kid: Option<String>,
    claims: Value,
    message: Vec<u8>,
    signature: Vec<u8>,
}

fn decoded(part: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD.decode(part).map_err(|_| "a part of the token is not base64url".to_owned())
}

/// Reads `token`.
///
/// # Errors
/// It is not a compact JWS, or names an algorithm not accepted here.
pub fn read(token: &str) -> Result<Signed, String> {
    if token.len() > MAX_TOKEN {
        return Err("the token is too large".into());
    }
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("the token is not a compact JWS".into());
    };
    let head: Value = serde_json::from_slice(&decoded(header)?)
        .map_err(|_| "the token's header is not JSON".to_owned())?;
    if head.get("crit").is_some() {
        return Err("the token names critical extensions".into());
    }
    let named = head.get("alg").and_then(Value::as_str).unwrap_or_default();
    let alg = Alg::named(named).ok_or_else(|| format!("the token is signed with {named:?}"))?;
    let claims = serde_json::from_slice(&decoded(payload)?)
        .map_err(|_| "the token's claims are not JSON".to_owned())?;
    Ok(Signed {
        alg,
        kid: head.get("kid").and_then(Value::as_str).map(str::to_owned),
        claims,
        message: format!("{header}.{payload}").into_bytes(),
        signature: decoded(signature)?,
    })
}

impl Signed {
    /// Whether some key it may be verified by does: that key, when it names one.
    #[must_use]
    pub fn verified_by(&self, keys: &[Key]) -> bool {
        keys.iter()
            .filter(|k| self.kid.is_none() || k.kid == self.kid)
            .any(|k| k.verifies(self.alg, &self.message, &self.signature))
    }

    /// Whether some key among `keys` is the one it names, so fetching the keys again cannot help.
    #[must_use]
    pub fn names_one_of(&self, keys: &[Key]) -> bool {
        self.kid.is_none() || keys.iter().any(|k| k.kid == self.kid)
    }

    /// Its claims, which only a caller that has verified it may believe.
    #[must_use]
    pub const fn claims(&self) -> &Value {
        &self.claims
    }
}
