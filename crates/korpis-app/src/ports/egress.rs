// SPDX-License-Identifier: AGPL-3.0-or-later

//! The egress budget's store (§5.1 of docs/05-scheduling.md, "The egress budget"): what each
//! workload sent in a month, where, and what the workloads of spent entries are held to.

use super::StoreResult;
use korpis_core::tenancy::quota::{Exhaustion, QuotaEntry, Spent};
use std::collections::BTreeMap;
use std::time::SystemTime;
use uuid::Uuid;

/// A workload on a node, in a month: where it runs now or sent from, and what it sent there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressAt {
    /// The workload.
    pub workload: Uuid,
    /// Its project.
    pub project: Uuid,
    /// Its organization.
    pub organization: Uuid,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
    /// The node; none for what was recorded with no node.
    pub node: Option<Uuid>,
    /// The node's labels.
    pub node_labels: BTreeMap<String, String>,
    /// The bytes its records of the month hold for it there, as an invoice sums them.
    pub bytes: u64,
    /// Whether it is placed there now, and so held there if it is to be held.
    pub placed: bool,
}

/// Reading the egress budget's state.
pub trait EgressRead {
    /// Every quota entry with an egress budget.
    fn budgeted_quota_entries(&self) -> impl Future<Output = StoreResult<Vec<QuotaEntry>>> + Send;

    /// Every workload placed now or with egress recorded from `from` to before `to`, on each
    /// node, with what it sent there.
    fn egress_at(
        &self,
        from: SystemTime,
        to: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<EgressAt>>> + Send;

    /// What each workload held is held to, and the month it was found in.
    fn workload_egress_all(
        &self,
    ) -> impl Future<Output = StoreResult<BTreeMap<Uuid, (SystemTime, Exhaustion)>>> + Send;

    /// What `workload` is held to in the month beginning at `month`, if anything.
    fn workload_egress(
        &self,
        workload: Uuid,
        month: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<Exhaustion>>> + Send;

    /// What each of `entries` was last reckoned to have spent.
    fn quota_egress_spent(
        &self,
        entries: &[Uuid],
    ) -> impl Future<Output = StoreResult<BTreeMap<Uuid, Spent>>> + Send;
}

/// Writing the egress budget's state.
pub trait EgressWrite {
    /// Records what `entry` was reckoned to have spent.
    fn put_quota_egress(
        &mut self,
        entry: Uuid,
        spent: &Spent,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Holds `workload` to `held` in the month beginning at `month`; announces it.
    fn put_workload_egress(
        &mut self,
        workload: Uuid,
        month: SystemTime,
        held: Exhaustion,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Holds `workload` to nothing; announces it if it was held.
    fn clear_workload_egress(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
