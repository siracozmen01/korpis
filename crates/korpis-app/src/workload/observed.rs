// SPDX-License-Identifier: AGPL-3.0-or-later

//! Measuring a workload against what its node reports (§4.3 of docs/02-architecture.md). An
//! observation is current only while it comes from the node the workload is placed on and that
//! node is heard; otherwise it is shown as what it is, the last thing known, and convergence is
//! `unknown` (P4).

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::node::Connectivity;
use korpis_core::workload::observation::Observation;
use korpis_core::workload::status::{Convergence, Reported, convergence};
use korpis_core::workload::{Plan, Workload};
use uuid::Uuid;

/// A workload's latest observation, and whether it is current.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    /// The observation.
    pub observation: Observation,
    /// Whether it is current: from the node the workload is placed on, while that node is heard.
    pub current: bool,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The workload's convergence, and its observation as the status shows it.
    pub(super) async fn measure(
        &self,
        w: &Workload,
        open: &[Plan],
        placed_on: Option<Uuid>,
        placed: bool,
    ) -> Result<(Convergence, Option<Observed>), AppError> {
        let now = self.clock.now();
        let Some(observation) = self.store.observation(w.id).await? else {
            return Ok((convergence(w, open, placed, now, None), None));
        };
        let heard = self
            .store
            .node(observation.node)
            .await?
            .is_some_and(|n| n.connectivity == Connectivity::Connected);
        let current = heard && placed_on == Some(observation.node);
        let generation = self.store.assignment_generation(w.id, observation.node).await?;
        let power = self.store.power_at(w.id, w.power_target_version).await?;
        let reported = match (generation, &power) {
            (Some(generation), Some(p)) => Some(Reported {
                observation: &observation,
                fresh: current,
                generation,
                power: &p.power,
            }),
            _ => None,
        };
        let c = convergence(w, open, placed, now, reported);
        Ok((c, Some(Observed { observation, current })))
    }
}
