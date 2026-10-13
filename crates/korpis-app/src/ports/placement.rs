// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placements (§3.4 of docs/01-model.md, §2.3 of docs/05-scheduling.md).

use super::StoreResult;
use crate::page::Page;
use korpis_core::node::Node;
use korpis_core::placement::{Placement, Unplaced};
use korpis_core::schedule::Peer;
use korpis_core::tenancy::quota::Bounds;
use std::collections::BTreeMap;
use std::future::Future;
use uuid::Uuid;

/// Reading where workloads are.
pub trait PlacementRead {
    /// A workload's authoritative placement.
    fn placement(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Placement>>> + Send;

    /// Why a workload's target has no node, if the newest attempt found none.
    fn unplaced(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Unplaced>>> + Send;

    /// Workloads not deleted whose approved target has no authoritative placement bound for
    /// it, in identifier order after the page's cursor.
    fn awaiting_placement(&self, page: Page)
    -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;
}

/// Binding workloads to nodes.
pub trait PlacementWrite {
    /// The grant of the plan whose decision `workload`'s placement and lease carry out: the plan
    /// that approved its current target, or with none the plan most recently approved. The
    /// control plane's own effects name it (§3.2 of docs/01-model.md).
    fn decision_grant(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Uuid>>> + Send;

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

    /// The nodes any of `owners` owns, decommissioned ones excepted.
    fn serving_nodes(
        &mut self,
        owners: &[Uuid],
    ) -> impl Future<Output = StoreResult<Vec<Node>>> + Send;

    /// The placed workloads of `organization`, `except` one, where each is placed
    /// authoritatively: what affinity and spread count.
    fn peers(
        &mut self,
        organization: Uuid,
        except: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<Peer>>> + Send;

    /// A workload's authoritative placement, as this transaction sees it.
    fn placement_in(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Placement>>> + Send;

    /// Writes a placement: a new one, or the existing one on the same node following its
    /// workload's target in place.
    fn bind(&mut self, placement: &Placement) -> impl Future<Output = StoreResult<()>> + Send;

    /// Removes every placement of a workload and any record of it being unplaced: it has no
    /// approved target any more.
    fn unbind(&mut self, workload: Uuid) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records the newest attempt that found no node. True if no attempt for the same intent
    /// was recorded before, so the failure is logged once per target, not once per retry.
    fn record_unplaced(
        &mut self,
        unplaced: &Unplaced,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Forgets that a workload was unplaced.
    fn clear_unplaced(&mut self, workload: Uuid) -> impl Future<Output = StoreResult<()>> + Send;
}
