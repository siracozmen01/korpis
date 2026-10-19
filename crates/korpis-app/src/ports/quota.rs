// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota held at admission (§5.1 of docs/05-scheduling.md), and the totals kept beside each entry
//! (question 7 of §10 there).

use super::StoreResult;
use korpis_core::tenancy::quota::{Held, QuotaEntry, QuotaOwner, Totals};
use std::collections::BTreeMap;
use std::future::Future;
use uuid::Uuid;

/// What a workload was last counted as holding, and by which entries: exactly what those
/// entries' totals hold of it, so that taking it back out is exact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counted {
    /// What it holds.
    pub held: Held,
    /// The entries counting it.
    pub by: Vec<Uuid>,
}

/// A workload under a quota owner, with what it was last counted as holding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holding {
    /// The workload.
    pub workload: Uuid,
    /// Its labels, for selectors.
    pub labels: BTreeMap<String, String>,
    /// What it was last counted as holding.
    pub counted: Counted,
}

/// What the quota reconciliation walks.
pub trait QuotaRead {
    /// Every quota entry.
    fn quota_entry_ids(&self) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// Every workload that holds or may hold quota: those not deleted, and those deleted that are
    /// still counted by an entry.
    fn workloads_to_count(&self) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;
}

/// Admission's side of quota. Every method runs in the transaction of the declaration it admits.
pub trait QuotaWrite {
    /// The entries of `project` and of `organizations`, locked until the transaction ends, in
    /// identifier order so that two admissions never wait on each other in a cycle. Locking
    /// them is what makes the check hold: a concurrent admission against the same entries waits
    /// for this one and then sees what it admitted.
    fn lock_quota(
        &mut self,
        project: Uuid,
        organizations: &[Uuid],
    ) -> impl Future<Output = StoreResult<Vec<QuotaEntry>>> + Send;

    /// Bumps the version of every entry in `entries`: the record, in the admitting
    /// transaction, that they were consumed against (§5.1).
    fn consume_quota(&mut self, entries: &[Uuid]) -> impl Future<Output = StoreResult<()>> + Send;

    /// Entry `id`, locked until the transaction ends.
    fn lock_quota_entry(
        &mut self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<QuotaEntry>>> + Send;

    /// Sets the totals of entry `entry`, leaving its version: totals follow what was admitted.
    fn put_quota_totals(
        &mut self,
        entry: Uuid,
        totals: &Totals,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// What `workload` was last counted as holding, and by which entries; `None` if it never
    /// was.
    fn counted(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Counted>>> + Send;

    /// Records what `workload` is counted as holding, and by which entries.
    fn put_counted(
        &mut self,
        workload: Uuid,
        counted: &Counted,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Every workload not deleted under `owner` (an organization's includes its descendants')
    /// that has been counted, locked until the transaction ends: what an entry new to them is
    /// reckoned from.
    fn holdings(
        &mut self,
        owner: QuotaOwner,
    ) -> impl Future<Output = StoreResult<Vec<Holding>>> + Send;

    /// Adds entry `entry` to the entries counting each of `workloads`.
    fn count_in(
        &mut self,
        entry: Uuid,
        workloads: &[Uuid],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// What each workload counted by entry `entry` holds, as it was counted.
    fn counted_by(&mut self, entry: Uuid) -> impl Future<Output = StoreResult<Vec<Held>>> + Send;
}
