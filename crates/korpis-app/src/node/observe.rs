// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recording what a node observes (§4.3 of docs/02-architecture.md). An observation is believed
//! only for a workload the node holds an assignment or placement for: a node cannot report on a
//! workload it was never told about.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, ObservationWrite, RecipeSource, Store, Tx};
use korpis_core::workload::observation::Observation;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Records `observation` from `node` as the workload's latest.
    pub async fn observe(&self, node: Uuid, observation: Observation) -> Result<(), AppError> {
        if observation.node != node {
            return Err(AppError::invalid("observation.node", "a node reports for itself"));
        }
        if !self.store.nodes_of(observation.workload).await?.contains(&node) {
            return Err(AppError::invalid(
                "observation.workload",
                "the workload is not assigned to this node",
            ));
        }
        let mut tx = self.store.begin().await?;
        tx.record_observation(&observation, self.clock.now()).await?;
        tx.commit().await?;
        Ok(())
    }
}
