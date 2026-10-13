// SPDX-License-Identifier: AGPL-3.0-or-later

//! The latest observation per workload (§4.3 of docs/02-architecture.md).

use super::StoreResult;
use korpis_core::workload::observation::Observation;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading observations, and what they are measured against.
pub trait ObservationRead {
    /// The latest observation of a workload, from whichever node made it.
    fn observation(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Observation>>> + Send;

    /// The generation of `node`'s active assignment for `workload`, if it holds one.
    fn assignment_generation(
        &self,
        workload: Uuid,
        node: Uuid,
    ) -> impl Future<Output = StoreResult<Option<u64>>> + Send;
}

/// Recording observations.
pub trait ObservationWrite {
    /// Replaces the workload's latest observation, received at `at`.
    fn record_observation(
        &mut self,
        observation: &Observation,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
