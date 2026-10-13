// SPDX-License-Identifier: AGPL-3.0-or-later

//! Leases (§4.5 of docs/02-architecture.md).

use super::StoreResult;
use korpis_core::workload::lease::Lease;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Writing leases; every method runs in the transaction that decides.
pub trait LeaseWrite {
    /// A workload's lease, locked until the transaction ends.
    fn lease_for_update(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Lease>>> + Send;

    /// The next epoch: one past every epoch ever issued and past the floor a restore set
    /// (§8 of docs/03-state.md).
    fn next_epoch(&mut self) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Writes a workload's lease, replacing the one it had.
    fn put_lease(&mut self, lease: &Lease) -> impl Future<Output = StoreResult<()>> + Send;

    /// Extends the lease from `received_at`.
    fn renew_lease(
        &mut self,
        workload: Uuid,
        received_at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that `node` stopped acting under `epoch`; false unless that is the lease.
    fn release_lease(
        &mut self,
        workload: Uuid,
        node: Uuid,
        epoch: u64,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
