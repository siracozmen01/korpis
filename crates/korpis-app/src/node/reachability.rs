// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the control plane learns of a node beyond its heartbeat: whether it can be reached at
//! what it advertises (§9 of docs/07-networking.md), and which recipes' content it holds (§2.2
//! of docs/05-scheduling.md). For reachability, the control plane decides when a node is due,
//! the prober measures it, and the result is recorded only if the node still advertises what was
//! measured: a measurement of addresses it has since given up says nothing about the ones it has
//! now.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, SessionWrite as _, Store, Tx as _};
use korpis_core::node::Reachability;
use korpis_core::node::advertised::Advertised;
use std::time::Duration;
use uuid::Uuid;

/// The most recipe digests one content report may name: far more distinct recipes than a node
/// runs, and few enough that a report is one bounded write.
pub const CONTENT_REPORTED: usize = 65_536;

/// What a probe of a node needs: what to measure, and the key its answers are signed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeDue {
    /// The addresses and ports it advertises.
    pub advertised: Advertised,
    /// Its public key, raw.
    pub key: Vec<u8>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// What to probe on `node`, if it advertises an address and has not been measured within
    /// `every`; none otherwise.
    pub async fn probe_due(
        &self,
        node: Uuid,
        every: Duration,
    ) -> Result<Option<ProbeDue>, AppError> {
        let Some(n) = self.store.node(node).await? else { return Ok(None) };
        let Some(advertised) = n.advertised.filter(|a| !a.addresses.is_empty()) else {
            return Ok(None);
        };
        let now = self.clock.now();
        let fresh = n.probed_at.is_some_and(|at| now.duration_since(at).is_ok_and(|d| d < every));
        if fresh {
            return Ok(None);
        }
        let key = self.store.node_key(node).await?.ok_or(AppError::NotFound { kind: "node" })?;
        Ok(Some(ProbeDue { advertised, key }))
    }

    /// Records the recipes whose content `node` holds, as its agent reported them, replacing
    /// what it reported before (§2.2 of docs/05-scheduling.md). A report naming anything but
    /// canonical digests, or more than [`CONTENT_REPORTED`] of them, is refused whole.
    pub async fn record_content(&self, node: Uuid, present: &[String]) -> Result<(), AppError> {
        if present.len() > CONTENT_REPORTED {
            return Err(AppError::invalid("content_status.present", "too many digests"));
        }
        if !present.iter().all(|d| korpis_core::recipe::reference::is_digest(d)) {
            return Err(AppError::invalid("content_status.present", "a digest is not canonical"));
        }
        let mut tx = self.store.begin().await?;
        tx.record_content(node, present, self.clock.now()).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records `node`'s reachability as measured against `measured`; whether it still applied.
    pub async fn record_reachability(
        &self,
        node: Uuid,
        measured: &Advertised,
        reachability: Reachability,
    ) -> Result<bool, AppError> {
        let mut tx = self.store.begin().await?;
        let recorded =
            tx.record_reachability(node, measured, reachability, self.clock.now()).await?;
        tx.commit().await?;
        Ok(recorded)
    }
}
