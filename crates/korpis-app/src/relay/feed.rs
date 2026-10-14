// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a joined relay is fed (§3.1 of docs/02-architecture.md). A relay opens its feed with a
//! request signed by its identity key, fresh by the auth skew window and never replayed, and is
//! then sent what it verifies by: the token keys, the revocation list, and the nodes' identity
//! keys. None of it is secret from the relay: the relay is operator infrastructure (§8 of
//! docs/17-security.md); the signature is what keeps anyone else from reading the installation's
//! node list.

use crate::App;
use crate::error::AppError;
use crate::node::NodeProof;
use crate::node::session::{ProofMessage, fresh};
use crate::ports::{Clock, RecipeSource, RelayWrite as _, Store, Tx as _, VerifyKey};
use korpis_core::authz::route::RouteKey;
use korpis_core::node::identity::{relay_feed_message, verify};
use korpis_core::relay::Relay;
use std::time::UNIX_EPOCH;
use uuid::Uuid;

/// What a relay verifies by, as it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayFeed {
    /// Every key that ever signed a capability token.
    pub token_keys: Vec<VerifyKey>,
    /// The identity key of every node not decommissioned.
    pub node_keys: Vec<(Uuid, Vec<u8>)>,
    /// The route key of the token key in use, which every replica holds alike: what a relay
    /// opens the routes this control plane seals with (§3.1 of docs/02-architecture.md).
    pub route_keys: Vec<RouteKey>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The relay `proof` was signed for, if it is that relay's fresh signature over a feed
    /// request and the relay is not removed.
    pub async fn authenticate_relay(
        &self,
        relay: Uuid,
        proof: &NodeProof,
    ) -> Result<Relay, AppError> {
        self.check_relay(relay, proof, &relay_feed_message).await
    }

    /// The relay `proof` was signed for, if it is that relay's fresh signature over `message`'s
    /// form, with a nonce never seen from it, and the relay is not removed.
    pub(super) async fn check_relay(
        &self,
        relay: Uuid,
        proof: &NodeProof,
        message: ProofMessage<'_>,
    ) -> Result<Relay, AppError> {
        let now = self.clock.now();
        let skew = self.session_terms.auth_skew;
        fresh(proof, (now, skew))?;
        let found = self.store.relay(relay).await?.filter(|r| r.removed_at.is_none());
        let found = found.ok_or(AppError::Unauthenticated)?;
        let key = self.store.relay_key(relay).await?.ok_or(AppError::Unauthenticated)?;
        let message = message(&proof.signed_node, proof.seconds, proof.nanos, &proof.nonce);
        if !verify(&key, &message, &proof.signature) {
            return Err(AppError::Unauthenticated);
        }
        let forget = now.checked_sub(skew.saturating_mul(2)).unwrap_or(UNIX_EPOCH);
        let mut tx = self.store.begin().await?;
        if !tx.remember_relay_nonce(relay, &proof.nonce, now, forget).await? {
            return Err(AppError::Unauthenticated);
        }
        tx.commit().await?;
        Ok(found)
    }

    /// What relays verify by now.
    pub async fn relay_feed(&self) -> Result<RelayFeed, AppError> {
        Ok(RelayFeed {
            token_keys: self.store.verify_keys("token").await?,
            node_keys: self.store.node_keys().await?,
            route_keys: self.route_keys(),
        })
    }

    /// The route keys a relay in this process opens routes with.
    #[must_use]
    pub fn route_keys(&self) -> Vec<RouteKey> {
        vec![RouteKey::of(&self.token_key)]
    }

    /// Records that `relay`'s feed is being served now.
    pub async fn relay_seen(&self, relay: Uuid) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.relay_seen(relay, self.clock.now()).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Whether `relay` is still joined: not removed.
    pub async fn relay_joined(&self, relay: Uuid) -> Result<bool, AppError> {
        Ok(self.store.relay(relay).await?.is_some_and(|r| r.removed_at.is_none()))
    }
}
