// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node attaching its data connection to the relay this control plane serves (§3.1 of
//! docs/02-architecture.md). It proves the node's key as a Hello does, over a message of its own
//! form, so neither proof can stand in for the other, and its nonce joins the same memory, so
//! neither can be replayed as itself either.

use super::NodeProof;
use super::session::fresh;
use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, SessionWrite as _, Store, Tx as _};
use korpis_core::node::NodeAdmin;
use korpis_core::node::identity::{data_hello_message, verify};
use korpis_core::relay::Relay;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Whether `proof` is the fresh signature of the node whose identity key is `key` over a
/// `DataHello` for `relay`: what a relay running as its own process checks, with the key its feed
/// sent it and its own clock. The nonce is the caller's to remember.
pub fn check_data_hello(
    key: &[u8],
    proof: &NodeProof,
    relay: &str,
    at: (SystemTime, Duration),
) -> Result<(), AppError> {
    fresh(proof, at)?;
    let message =
        data_hello_message((&proof.signed_node, relay), proof.seconds, proof.nanos, &proof.nonce);
    if verify(key, &message, &proof.signature) { Ok(()) } else { Err(AppError::Unauthenticated) }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Admits `node`'s data connection to the relay role this control plane serves, if `proof`
    /// is its fresh signature over a `DataHello` for `relay`, the URL it dialled, and the node is
    /// not decommissioned. A hello for the URL of any relay enrolled to run as its own process,
    /// removed ones included, is refused: it was made for that relay, which may present it.
    pub async fn attach_node(
        &self,
        node: Uuid,
        proof: &NodeProof,
        relay: &str,
    ) -> Result<(), AppError> {
        if self.store.relays().await?.iter().any(|r| r.url.as_str() == relay) {
            return Err(AppError::Unauthenticated);
        }
        let now = self.clock.now();
        let message = |n: &str, s, ns, nonce: &[u8]| data_hello_message((n, relay), s, ns, nonce);
        self.check_proof(node, proof, (now, &message)).await?;
        let forget = now.checked_sub(self.session_terms.auth_skew.saturating_mul(2));
        let mut tx = self.store.begin().await?;
        if !tx.remember_nonce(node, &proof.nonce, now, forget.unwrap_or(UNIX_EPOCH)).await? {
            return Err(AppError::Unauthenticated);
        }
        tx.commit().await?;
        let record = self.store.node(node).await?.ok_or(AppError::Unauthenticated)?;
        if record.admin == NodeAdmin::Decommissioned {
            return Err(AppError::Unauthenticated);
        }
        Ok(())
    }

    /// The URLs of the relays enrolled to run as their own process and admitted, neither removed
    /// nor held by a restore, which every node keeps a data connection to (§3.1 of
    /// docs/02-architecture.md).
    pub async fn relay_urls(&self) -> Result<Vec<String>, AppError> {
        let relays = self.store.relays().await?;
        let live = relays.into_iter().filter(Relay::admitted);
        Ok(live.map(|r| r.url.as_str().to_owned()).collect())
    }

    /// The window either side of now a node's or a relay's signed timestamp must fall in.
    #[must_use]
    pub fn auth_skew(&self) -> Duration {
        self.session_terms.auth_skew
    }
}
