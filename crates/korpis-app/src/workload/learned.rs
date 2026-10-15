// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the scheduler learns of nodes over time (§2.2 of docs/05-scheduling.md), read for one
//! placement: which nodes reported holding the target's recipe, and what each node's recent loss
//! events weigh now.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, PlacementWrite, RecipeSource, Store};
use korpis_core::schedule::churn::{CHURN_MEMORY, loss_weight};
use korpis_core::workload::Intent;
use std::collections::BTreeMap;
use std::time::UNIX_EPOCH;
use uuid::Uuid;

/// What scoring learns of a placement's candidates.
pub(super) struct Learned {
    /// Each node that reported its content, with whether it holds the target's recipe.
    pub(super) content: BTreeMap<Uuid, bool>,
    /// Each node with losses it remembers, with what they weigh now.
    pub(super) churn: BTreeMap<Uuid, f64>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// What scoring learns of `nodes` for `target`, read in `tx`.
    pub(super) async fn learned(
        &self,
        tx: &mut S::Tx,
        nodes: &[Uuid],
        target: &Intent,
    ) -> Result<Learned, AppError> {
        let content = tx.content_held(nodes, &target.body.recipe.digest).await?;
        let now = self.clock.now();
        let since = now.checked_sub(CHURN_MEMORY).unwrap_or(UNIX_EPOCH);
        let churn = tx
            .losses(nodes, since)
            .await?
            .into_iter()
            .map(|(node, at)| {
                let ages = at.into_iter().map(|t| now.duration_since(t).unwrap_or_default());
                (node, loss_weight(ages))
            })
            .collect();
        Ok(Learned { content, churn })
    }
}
