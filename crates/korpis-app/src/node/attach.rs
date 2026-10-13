// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node attaching its data connection to the relay this control plane serves (§3.1 of
//! docs/02-architecture.md). It proves the node's key as a Hello does, over a message of its own
//! form, so neither proof can stand in for the other, and its nonce joins the same memory, so
//! neither can be replayed as itself either.

use super::NodeProof;
use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, SessionWrite as _, Store, Tx as _};
use korpis_core::node::NodeAdmin;
use korpis_core::node::identity::data_hello_message;
use std::time::UNIX_EPOCH;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Admits `node`'s data connection if `proof` is its fresh signature over a `DataHello` and
    /// the node is not decommissioned.
    pub async fn attach_node(&self, node: Uuid, proof: &NodeProof) -> Result<(), AppError> {
        let now = self.clock.now();
        self.check_proof(node, proof, (now, data_hello_message)).await?;
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
}
