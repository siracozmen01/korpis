// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nodes (§3.4 of docs/01-model.md).

use super::StoreResult;
use crate::page::Page;
use korpis_core::node::{DriverInfo, Node};
use korpis_core::tenancy::quota::Bounds;
use std::future::Future;
use uuid::Uuid;

/// Reading nodes.
pub trait NodeRead {
    /// One node, decommissioned or not.
    fn node(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Node>>> + Send;

    /// The nodes an organization owns, in identifier order after the page's cursor.
    fn nodes(
        &self,
        organization: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Node>>> + Send;

    /// What the placements on a node reserve, summed per dimension: its `allocated`.
    fn allocated(&self, node: Uuid) -> impl Future<Output = StoreResult<Bounds>> + Send;

    /// The drivers of every node any of `owners` owns, decommissioned ones excepted: what a
    /// workload of those organizations could run on.
    fn serving_drivers(
        &self,
        owners: &[Uuid],
    ) -> impl Future<Output = StoreResult<Vec<DriverInfo>>> + Send;
}

/// Changing nodes.
pub trait NodeWrite {
    /// Records a newly enrolled node. Enrollment itself (§3 of docs/18-operations.md) is the
    /// agent protocol's; this is where it lands, with the public half of the key the node
    /// generated for itself (§7 of docs/02-architecture.md).
    fn enroll_node(
        &mut self,
        node: &Node,
        public_key: &[u8],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// One node, locked until the transaction ends.
    fn lock_node(&mut self, id: Uuid) -> impl Future<Output = StoreResult<Option<Node>>> + Send;

    /// Writes the operator's decision if the node is still at `expected` version, bumping it;
    /// false if it moved.
    fn update_node_admin(
        &mut self,
        node: &Node,
        expected: u64,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// What `node` holds, as this transaction sees it: the workloads placed on it, and how many
    /// volumes on it are not deleted.
    fn node_holdings(
        &mut self,
        node: Uuid,
    ) -> impl Future<Output = StoreResult<(Vec<Uuid>, u64)>> + Send;

    /// Removes every endpoint placed on `node`; how many there were.
    fn drop_node_endpoints(&mut self, node: Uuid) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Tells every replica, at commit, to recompute every session's set: a change to a node that
    /// no workload's change announces.
    fn announce_resync(&mut self) -> impl Future<Output = StoreResult<()>> + Send;
}
