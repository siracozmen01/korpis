// SPDX-License-Identifier: AGPL-3.0-or-later

//! Retiring workloads (§3.3 of docs/01-model.md, §4.8 of docs/02-architecture.md).

use super::StoreResult;
use korpis_core::workload::Plan;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// The writes a retirement makes.
pub trait RetirementWrite {
    /// Whether a retire plan of `workload` is approved, queued, or applying: the workload is
    /// being retired, and nothing else may be declared or approved for it.
    fn retiring(&mut self, workload: Uuid) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Marks every proposed plan of `keep`'s workload but `keep` superseded, whatever its kind,
    /// and returns how many there were: nothing but the retirement can be approved any more.
    fn supersede_all_proposed(
        &mut self,
        keep: &Plan,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Retires the workload's identity at `at`: it is deleted, its name is free, its quota is
    /// released, every volume attached to it is detached, kept with its data (§7 of
    /// docs/06-storage.md), and every name it routes is free (§3.2 of docs/07-networking.md).
    /// False if it was already retired. Its identifier is kept forever.
    fn retire_workload(
        &mut self,
        workload: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
