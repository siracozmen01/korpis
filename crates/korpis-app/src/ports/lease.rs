// SPDX-License-Identifier: AGPL-3.0-or-later

//! Leases (§4.5 of docs/02-architecture.md).

use super::StoreResult;
use korpis_core::workload::lease::Lease;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// What asking for the next epoch came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochIssue {
    /// This epoch, issued.
    Issued(u64),
    /// None: a restored control plane is settling, and issues nothing until it ends.
    Settling,
    /// None: the issue rate is spent for now.
    RateSpent,
}

/// Writing leases; every method runs in the transaction that decides.
pub trait LeaseWrite {
    /// A workload's lease, locked until the transaction ends.
    fn lease_for_update(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Lease>>> + Send;

    /// The next epoch: one past every epoch ever issued and past the floor a restore set
    /// (§8 of docs/03-state.md), unless a restore is settling or the issue rate is spent, at
    /// `now`.
    fn next_epoch(
        &mut self,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<EpochIssue>> + Send;

    /// Raises the epoch floor to at least `floor` and refuses to issue until `settling_until`:
    /// a restore's first fence (§8 of docs/03-state.md).
    fn fence_epochs(
        &mut self,
        floor: u64,
        settling_until: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Raises the epoch floor to at least `floor`, and records a settling window of `settle` that
    /// the control plane's next start opens: a restore run while it is stopped.
    fn fence_epochs_pending(
        &mut self,
        floor: u64,
        settle: std::time::Duration,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Opens a pending settling window at `now`; until when it settles, if one was pending.
    fn open_pending_settle(
        &mut self,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<SystemTime>>> + Send;

    /// Raises the epoch floor to at least `epoch`, past an epoch a node proved was issued;
    /// whether it rose.
    fn raise_epoch_floor(&mut self, epoch: u64) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Until when a restore settles, if it does at `now`.
    fn settling(
        &mut self,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<SystemTime>>> + Send;

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
