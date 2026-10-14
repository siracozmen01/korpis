// SPDX-License-Identifier: AGPL-3.0-or-later

//! Routes: how a relay learns which node to carry a session to without the client learning it
//! (§3.1 of docs/02-architecture.md). The gateway, which knows where a workload is placed, seals
//! the node's identifier when it mints a session token, and the client hands the sealed route to
//! the relay with the token. The relay opens it and asks that node. A tenant who may open a
//! session with a workload has no authority to read the node it runs on, and a route tells it
//! nothing: each is sealed under a fresh nonce, so two routes to the same node do not compare
//! equal, and the client cannot learn which of its workloads share a machine.
//!
//! The route key is derived from the token key and shares its fate: rotating the token key, as
//! every restore does, retires every route sealed under it, exactly as it retires every token.
//! The control plane holds it by holding the token key; a relay is given it in its feed, beside
//! the token keys it verifies with.

use super::token::SigningKey;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use std::collections::BTreeMap;
use uuid::Uuid;

/// The form a route is written in, so a later one can be told apart.
const PREFIX: &str = "r1";

/// Why a route did not open.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    /// Not a route, or one for another workload, or sealed under a key not held.
    #[error("not a route to this workload under a key held here")]
    Invalid,
    /// The operating system could not supply a nonce.
    #[error("randomness: {0}")]
    Random(String),
}

/// The key routes are sealed with.
#[derive(Clone, PartialEq, Eq)]
pub struct RouteKey {
    /// The token key's epoch it was derived from.
    pub key_id: u32,
    secret: [u8; 32],
}

impl std::fmt::Debug for RouteKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteKey").field("key_id", &self.key_id).finish_non_exhaustive()
    }
}

impl RouteKey {
    /// The route key of `token`, derived from its private half under a label of its own, so it
    /// is never the token key put to a second use.
    #[must_use]
    pub fn of(token: &SigningKey) -> Self {
        let info = format!("korpis.route.v1|{}", token.key_id);
        let mut secret = [0u8; 32];
        // A 32-byte output is far below what HKDF-SHA256 can produce, so expansion cannot fail.
        let _ =
            Hkdf::<Sha256>::new(None, &token.private_bytes()).expand(info.as_bytes(), &mut secret);
        Self { key_id: token.key_id, secret }
    }

    /// A key a relay was given: its epoch and its 32 bytes.
    #[must_use]
    pub fn from_bytes(key_id: u32, bytes: [u8; 32]) -> Self {
        Self { key_id, secret: bytes }
    }

    /// Its 32 bytes, for a relay's feed.
    #[must_use]
    pub const fn bytes(&self) -> [u8; 32] {
        self.secret
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(&Key::from(self.secret))
    }

    /// `node`, sealed for a session with `workload`.
    pub fn seal(&self, workload: Uuid, node: Uuid) -> Result<String, RouteError> {
        let mut nonce = [0u8; 12];
        getrandom::fill(&mut nonce).map_err(|e| RouteError::Random(e.to_string()))?;
        let payload = Payload { msg: node.as_bytes(), aad: workload.as_bytes() };
        let sealed =
            self.cipher().encrypt(&Nonce::from(nonce), payload).map_err(|_| RouteError::Invalid)?;
        let mut bytes = nonce.to_vec();
        bytes.extend(sealed);
        Ok(format!("{PREFIX}.{}.{}", self.key_id, URL_SAFE_NO_PAD.encode(bytes)))
    }

    fn open_bytes(&self, workload: Uuid, bytes: &[u8]) -> Result<Uuid, RouteError> {
        let (nonce, sealed) = bytes.split_at_checked(12).ok_or(RouteError::Invalid)?;
        let nonce: [u8; 12] = nonce.try_into().map_err(|_| RouteError::Invalid)?;
        let payload = Payload { msg: sealed, aad: workload.as_bytes() };
        let node =
            self.cipher().decrypt(&Nonce::from(nonce), payload).map_err(|_| RouteError::Invalid)?;
        Uuid::from_slice(&node).map_err(|_| RouteError::Invalid)
    }
}

/// The route keys a relay holds, by epoch, as it holds token keys during a rotation.
#[derive(Debug, Clone, Default)]
pub struct RouteRing {
    keys: BTreeMap<u32, RouteKey>,
}

impl RouteRing {
    /// A ring of `keys`.
    #[must_use]
    pub fn new(keys: impl IntoIterator<Item = RouteKey>) -> Self {
        Self { keys: keys.into_iter().map(|k| (k.key_id, k)).collect() }
    }

    /// The node `route` names for a session with `workload`.
    pub fn open(&self, workload: Uuid, route: &str) -> Result<Uuid, RouteError> {
        let mut parts = route.splitn(3, '.');
        let (Some(PREFIX), Some(key_id), Some(body)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(RouteError::Invalid);
        };
        let key_id: u32 = key_id.parse().map_err(|_| RouteError::Invalid)?;
        let key = self.keys.get(&key_id).ok_or(RouteError::Invalid)?;
        let bytes = URL_SAFE_NO_PAD.decode(body).map_err(|_| RouteError::Invalid)?;
        key.open_bytes(workload, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_route_opens_for_its_workload_alone_and_says_nothing_to_its_holder() {
        let token = SigningKey::generate(3);
        let key = RouteKey::of(&token);
        let (workload, node) = (Uuid::now_v7(), Uuid::now_v7());
        let route = key.seal(workload, node).unwrap();
        let ring = RouteRing::new([RouteKey::from_bytes(3, key.bytes())]);
        assert_eq!(ring.open(workload, &route), Ok(node), "a relay given the key opens it");
        assert_eq!(ring.open(Uuid::now_v7(), &route), Err(RouteError::Invalid), "bound to it");
        assert_ne!(key.seal(workload, node).unwrap(), route, "a fresh nonce: routes never compare");
        assert!(!route.contains(&node.simple().to_string()));
        let rotated = RouteRing::new([RouteKey::of(&SigningKey::generate(4))]);
        assert_eq!(rotated.open(workload, &route), Err(RouteError::Invalid), "retired with it");
        assert_eq!(ring.open(workload, "r1.3.AAAA"), Err(RouteError::Invalid));
        let mut tampered = route.into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).unwrap();
        assert_eq!(ring.open(workload, &tampered), Err(RouteError::Invalid), "authenticated");
    }
}
