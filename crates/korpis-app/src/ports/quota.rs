// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota held at admission (§5.1 of docs/05-scheduling.md), and the totals kept beside each entry
//! (question 7 of §10 there).

use super::StoreResult;
use korpis_core::tenancy::quota::{
    Bounds, Dimension, Held, QuotaEntry, QuotaOwner, QuotaSet, Totals,
};
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
    /// Every quota entry, each with whether it names the nodes it counts.
    fn quota_entry_ids(&self) -> impl Future<Output = StoreResult<Vec<(Uuid, bool)>>> + Send;

    /// Every workload that holds or may hold quota: those not deleted, and those deleted that are
    /// still counted by an entry.
    fn workloads_to_count(&self) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// The quota set of `owner`, with what each entry's workloads hold.
    fn quota_set(&self, owner: QuotaOwner) -> impl Future<Output = StoreResult<QuotaSet>> + Send;
}

/// Setting quota sets (§5.1 of docs/05-scheduling.md). Every method runs in the transaction of
/// the change.
pub trait QuotaSetWrite {
    /// The entries of `organizations` and `projects`, locked until the transaction ends, in
    /// identifier order: the sets being changed, and those they are checked against.
    fn lock_quota_sets(
        &mut self,
        organizations: &[Uuid],
        projects: &[Uuid],
    ) -> impl Future<Output = StoreResult<Vec<QuotaEntry>>> + Send;

    /// The overcommit `owner` permits its children's limits, per dimension.
    fn overcommit(
        &mut self,
        owner: QuotaOwner,
    ) -> impl Future<Output = StoreResult<std::collections::BTreeMap<Dimension, u32>>> + Send;

    /// Replaces the overcommit `owner` permits its children's limits.
    fn put_overcommit(
        &mut self,
        owner: QuotaOwner,
        percent: &std::collections::BTreeMap<Dimension, u32>,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records a new entry, its totals not yet reckoned.
    fn insert_quota_entry(
        &mut self,
        e: &QuotaEntry,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Sets the bounds of entry `id`, a new version of it.
    fn update_quota_bounds(
        &mut self,
        id: Uuid,
        reservations: &Bounds,
        limits: &Bounds,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Deletes entry `id`; what counted it no longer does.
    fn delete_quota_entry(&mut self, id: Uuid) -> impl Future<Output = StoreResult<()>> + Send;

    /// The organizations directly beneath `organization`, and its projects.
    fn quota_children(
        &mut self,
        organization: Uuid,
    ) -> impl Future<Output = StoreResult<(Vec<Uuid>, Vec<Uuid>)>> + Send;
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

/// Where a workload's placement was counted, and as holding what, by the entries that name the
/// nodes they count (§5.1 of docs/05-scheduling.md, "Where a workload runs").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    /// The node it is placed on.
    pub node: Uuid,
    /// What its placement runs, and the entries counting it there.
    pub counted: Counted,
}

/// A placed workload under a quota owner: its labels, its node's, and where it was counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedHolding {
    /// The workload.
    pub workload: Uuid,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
    /// Its node's labels.
    pub node_labels: BTreeMap<String, String>,
    /// Where it was counted.
    pub placed: Placed,
}

/// The quota entries that name the nodes they count. Every method runs under the placement lock,
/// which every change to them holds, so they take no lock of their own.
pub trait PlacedQuotaWrite {
    /// The entries of `project` and of `organizations` that name the nodes they count.
    fn placed_quota(
        &mut self,
        project: Uuid,
        organizations: &[Uuid],
    ) -> impl Future<Output = StoreResult<Vec<QuotaEntry>>> + Send;

    /// Where `workload`'s placement was counted; `None` if it is counted nowhere.
    fn placement_held(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Placed>>> + Send;

    /// Records where `workload`'s placement is counted, or, with `None`, that it is not.
    fn put_placement_held(
        &mut self,
        workload: Uuid,
        placed: Option<&Placed>,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Every placed workload not deleted under `owner` that has been counted: what an entry new
    /// to them is reckoned from.
    fn placed_holdings(
        &mut self,
        owner: QuotaOwner,
    ) -> impl Future<Output = StoreResult<Vec<PlacedHolding>>> + Send;

    /// Adds entry `entry` to the entries counting the placement of each of `workloads`.
    fn count_placed_in(
        &mut self,
        entry: Uuid,
        workloads: &[Uuid],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// What each placement counted by entry `entry` holds, as it was counted.
    fn placed_counted_by(
        &mut self,
        entry: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<Held>>> + Send;
}
