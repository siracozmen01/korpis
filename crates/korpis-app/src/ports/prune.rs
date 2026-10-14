// SPDX-License-Identifier: AGPL-3.0-or-later

//! What pruning reads and writes (§5.3 and §5.5 of docs/06-storage.md): the backups a policy
//! judges, the ones being read, the access granted to a workload's prefix, the objects a prune
//! marked, and the sweep that holds the prefix while it deletes them.

use super::StoreResult;
use korpis_core::backup::retention::Candidate;
use korpis_core::extension::AccessMode;
use std::collections::BTreeSet;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// A workload's place in a repository.
pub type Prefix = (Uuid, Uuid);

/// Access to a workload's prefix, as recorded before it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessRecord {
    /// The repository and the workload.
    pub prefix: Prefix,
    /// The backup read, for read access.
    pub backup: Option<Uuid>,
    /// Reading or writing.
    pub mode: AccessMode,
    /// When it was asked for.
    pub granted_at: SystemTime,
    /// When it ends.
    pub expires_at: SystemTime,
}

/// Reading what a prune judges.
pub trait PruneRead {
    /// The backups of the workload in the repository that are not forgotten, as retention
    /// judges them.
    fn prune_candidates(
        &self,
        prefix: Prefix,
    ) -> impl Future<Output = StoreResult<Vec<Candidate>>> + Send;

    /// The backups of the workload in the repository being read at `now`: by an open plan that
    /// restores one, or with read access that has not ended.
    fn backups_being_read(
        &self,
        prefix: Prefix,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<BTreeSet<Uuid>>> + Send;

    /// When the last write access to the prefix ends, if any ends after `now`.
    fn writable_until(
        &self,
        prefix: Prefix,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<SystemTime>>> + Send;

    /// The names marked in the prefix by plan `plan` at or before `before`.
    fn marked(
        &self,
        prefix: Prefix,
        plan: Uuid,
        before: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<String>>> + Send;

    /// The backups of the workload in the repository that were forgotten.
    fn forgotten_backups(
        &self,
        prefix: Prefix,
    ) -> impl Future<Output = StoreResult<BTreeSet<Uuid>>> + Send;
}

/// Writing what a prune decides, in a transaction.
pub trait PruneWrite {
    /// Records access about to be asked for.
    fn record_access(
        &mut self,
        access: &AccessRecord,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Forgets `backups` at `at`, those not already forgotten; how many it forgot.
    fn forget_backups(
        &mut self,
        backups: &[Uuid],
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Marks `names` in the prefix for plan `plan` at `at`; a name already marked keeps its
    /// first mark, so its grace runs from when it was first found unreferenced.
    fn mark_unreferenced(
        &mut self,
        prefix_and_plan: (Prefix, Uuid),
        names: &[String],
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Removes the marks of `names` in the prefix: swept, or referenced again.
    fn unmark(
        &mut self,
        prefix: Prefix,
        names: &[String],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Takes the prefix for sweep `job`, after locking it against backups being proposed;
    /// whether it was free: no backup into it is open, and no other sweep holds it.
    fn hold_for_sweep(
        &mut self,
        prefix: Prefix,
        job: Uuid,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Lets the prefix go, if sweep `job` holds it.
    fn release_sweep(
        &mut self,
        prefix: Prefix,
        job: Uuid,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Whether a sweep holds the prefix, read under a lock a sweep taking it waits for, so a
    /// backup proposed in this transaction and a sweep beginning cannot both proceed.
    fn swept(&mut self, prefix: Prefix) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Forgets the record of access that ended before `before`.
    fn drop_ended_access(
        &mut self,
        before: SystemTime,
    ) -> impl Future<Output = StoreResult<u64>> + Send;
}
