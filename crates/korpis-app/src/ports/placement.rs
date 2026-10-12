// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placements (§3.4 of docs/01-model.md, §2.3 of docs/05-scheduling.md).

use super::StoreResult;
use korpis_core::tenancy::quota::Bounds;
use std::collections::BTreeMap;
use std::future::Future;
use uuid::Uuid;

/// Binding workloads to nodes.
pub trait PlacementWrite {
    /// Takes the placement lock until the transaction ends. Every bind holds it from reading the
    /// nodes to writing the placement, and so does every change to whether a node takes
    /// placements, so two binds never both count the same free capacity and no bind lands on a
    /// node cordoned while it was choosing.
    fn lock_placement(&mut self) -> impl Future<Output = StoreResult<()>> + Send;

    /// What the placements on each of `nodes` reserve, summed per dimension, as this
    /// transaction sees it. A node with no placement is absent.
    fn allocations(
        &mut self,
        nodes: &[Uuid],
    ) -> impl Future<Output = StoreResult<BTreeMap<Uuid, Bounds>>> + Send;
}
