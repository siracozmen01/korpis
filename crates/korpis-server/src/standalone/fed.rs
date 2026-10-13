// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a relay running as its own process verifies by (§3.1 of docs/02-architecture.md): only
//! what its feed sent, held in memory. Token keys and node keys replace what was held whole; a
//! revocation list is believed only once its signature verifies against the token keys held,
//! as an agent believes one (§6.3 of docs/08-identity.md). A `DataHello` is accepted only for
//! this relay's own URL, fresh by the control plane's skew window, and with a nonce this relay
//! has not seen inside twice that window: nonce memory is the relay's own, which is why the URL
//! is signed.
//!
//! What it does not do: with the feed lost, it verifies by the last of each it received, as the
//! agent does (§4.6 of docs/02-architecture.md); and before the first terms arrive it admits no
//! node at all.

use crate::api::error;
use crate::relay::{Verifier, token_ring};
use connectrpc::ConnectError;
use korpis_app::AppError;
use korpis_app::node::{NodeProof, check_data_hello};
use korpis_core::authz::token::KeyRing;
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::relay::v1 as relay;
use korpis_proto::korpis::relay::v1::__buffa::oneof::feed_update::Message as Update;
use korpis_proto::korpis::v1::ErrorCode;
use korpis_wire::duration_from;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// The control plane's terms, as the feed declared them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terms {
    /// The window either side of now a `DataHello`'s timestamp must fall in.
    pub auth_skew: Duration,
    /// The longest the feed goes without sending anything.
    pub heartbeat: Duration,
}

#[derive(Debug, Default)]
struct Held {
    terms: Option<Terms>,
    keys: KeyRing,
    revoked: HashSet<Uuid>,
    nodes: HashMap<Uuid, Vec<u8>>,
}

/// What the relay holds of its feed.
#[derive(Debug)]
pub struct Fed {
    url: String,
    held: RwLock<Held>,
    nonces: Mutex<HashMap<(Uuid, Vec<u8>), SystemTime>>,
}

fn node_keys(nodes: &relay::NodeKeys) -> Result<HashMap<Uuid, Vec<u8>>, String> {
    let parse = |k: &relay::NodeKey| {
        let id = Id::parse(IdKind::Node, &k.node).map_err(|e| format!("node_keys: {e}"))?;
        Ok((id.uuid(), k.public_key.clone()))
    };
    nodes.nodes.iter().map(parse).collect()
}

fn terms(t: &relay::FeedTerms) -> Result<Terms, String> {
    let field = |name: &str, d: Option<&buffa_types::google::protobuf::Duration>| {
        let d = d.ok_or_else(|| format!("terms.{name}: required"))?;
        duration_from(name, d).map_err(|e| e.to_string())
    };
    Ok(Terms {
        auth_skew: field("auth_skew", t.auth_skew.as_option())?,
        heartbeat: field("heartbeat_interval", t.heartbeat_interval.as_option())?,
    })
}

impl Fed {
    /// Nothing held yet, for the relay serving at `url`.
    #[must_use]
    pub fn new(url: String) -> Self {
        Self { url, held: RwLock::default(), nonces: Mutex::default() }
    }

    /// The terms, once the feed declared them.
    #[must_use]
    pub fn terms(&self) -> Option<Terms> {
        self.held.read().unwrap_or_else(PoisonError::into_inner).terms
    }

    /// Takes in one update; why it was refused, if it was. A refused update changes nothing.
    pub fn apply(&self, update: relay::FeedUpdate) -> Result<(), String> {
        let Some(message) = update.message else { return Ok(()) };
        let mut held = self.held.write().unwrap_or_else(PoisonError::into_inner);
        match message {
            Update::Terms(t) => held.terms = Some(terms(&t)?),
            Update::TokenKeys(k) => {
                held.keys =
                    token_ring(k.keys.iter().map(|k| (k.key_id.as_str(), &k.public_key[..])));
            }
            Update::RevocationList(raw) => {
                let signed =
                    korpis_wire::revocation::revocation_from_pb(&raw).map_err(|e| e.to_string())?;
                let list = signed.verify(&held.keys).map_err(|e| e.to_string())?;
                held.revoked = list.grants.into_iter().collect();
            }
            Update::NodeKeys(n) => held.nodes = node_keys(&n)?,
            Update::Heartbeat(_) => {}
        }
        Ok(())
    }

    /// Admits `node`'s data connection by what the feed sent, or says why not.
    fn admit(&self, node: Uuid, proof: &NodeProof, relay: &str) -> Result<(), ConnectError> {
        if relay != self.url {
            return Err(unauthenticated());
        }
        let (key, skew) = {
            let held = self.held.read().unwrap_or_else(PoisonError::into_inner);
            let Some(terms) = held.terms else {
                let message = "this relay has not been fed yet";
                return Err(error::session_refused(
                    ErrorCode::ERROR_CODE_UNAVAILABLE,
                    "relay.unfed",
                    message,
                ));
            };
            let key = held.nodes.get(&node).cloned().ok_or_else(unauthenticated)?;
            (key, terms.auth_skew)
        };
        let now = SystemTime::now();
        check_data_hello(&key, proof, relay, (now, skew)).map_err(error::from_app)?;
        if !self.remember(node, &proof.nonce, now, skew) {
            return Err(unauthenticated());
        }
        Ok(())
    }

    /// Remembers `node`'s `nonce` at `now`, forgetting what is older than twice `skew`; false if
    /// it was remembered already.
    fn remember(&self, node: Uuid, nonce: &[u8], now: SystemTime, skew: Duration) -> bool {
        let forget = now.checked_sub(skew.saturating_mul(2)).unwrap_or(UNIX_EPOCH);
        let mut nonces = self.nonces.lock().unwrap_or_else(PoisonError::into_inner);
        nonces.retain(|_, seen| *seen >= forget);
        nonces.insert((node, nonce.to_vec()), now).is_none()
    }
}

fn unauthenticated() -> ConnectError {
    error::from_app(AppError::Unauthenticated)
}

impl Verifier for Fed {
    fn keys(&self) -> KeyRing {
        self.held.read().unwrap_or_else(PoisonError::into_inner).keys.clone()
    }

    fn revoked(&self) -> HashSet<Uuid> {
        self.held.read().unwrap_or_else(PoisonError::into_inner).revoked.clone()
    }

    fn attach(
        &self,
        node: Uuid,
        proof: &NodeProof,
        relay: &str,
    ) -> impl Future<Output = Result<(), ConnectError>> + Send {
        // Nothing here waits: the feed's state is in memory.
        std::future::ready(self.admit(node, proof, relay))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nonce_is_accepted_once_inside_twice_the_window() {
        let fed = Fed::new("https://r1.example".into());
        let (node, skew, now) = (Uuid::now_v7(), Duration::from_secs(30), SystemTime::now());
        assert!(fed.remember(node, &[1; 16], now, skew));
        assert!(!fed.remember(node, &[1; 16], now, skew), "a replay");
        assert!(fed.remember(Uuid::now_v7(), &[1; 16], now, skew), "another node's");
        let later = now + Duration::from_secs(61);
        assert!(fed.remember(node, &[2; 16], later, skew));
        assert!(fed.remember(node, &[1; 16], later, skew), "forgotten after twice the window");
    }

    #[test]
    fn a_list_that_does_not_verify_changes_nothing() {
        let fed = Fed::new("https://r1.example".into());
        let list = korpis_proto::korpis::agent::v1::RevocationList {
            grants: vec![Uuid::now_v7().to_string()],
            key_id: "token-1".into(),
            signature: vec![0; 64],
            ..Default::default()
        };
        let update = relay::FeedUpdate {
            message: Some(Update::RevocationList(Box::new(list))),
            ..Default::default()
        };
        assert!(fed.apply(update).is_err());
        assert!(fed.revoked().is_empty());
    }
}
