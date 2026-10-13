// SPDX-License-Identifier: AGPL-3.0-or-later

//! Recording what a node observes (§4.3 of docs/02-architecture.md). An observation is believed
//! only for a workload the node holds an assignment or placement for: a node cannot report on a
//! workload it was never told about. A change of the application's health from the last
//! observation is an event, written with the observation (§5 of docs/15-observability.md).

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, ConditionWrite, ObservationWrite, RecipeSource, Store, Tx};
use korpis_core::event::{Condition, ConditionChange};
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
        let before = self.store.observation(observation.workload).await?.map(|o| o.health);
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        tx.record_observation(&observation, now).await?;
        if let Some(from) = before.filter(|h| *h != observation.health) {
            let change = ConditionChange::Health { from, to: observation.health };
            let condition = Condition { workload: observation.workload, at: now, change };
            tx.append_condition(&condition).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
