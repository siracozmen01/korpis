// SPDX-License-Identifier: AGPL-3.0-or-later

//! A volume's replicas (§2.2 of docs/06-storage.md).

use super::StoreResult;
use korpis_core::workload::replica::Replica;
use korpis_core::workload::transfers::{Flow, Promoting};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading a volume's replicas.
pub trait ReplicaRead {
    /// The replicas of `volume`, by node.
    fn replicas_of(&self, volume: Uuid) -> impl Future<Output = StoreResult<Vec<Replica>>> + Send;

    /// Every replica of a live volume on one node's disk, with the volume's node and the
    /// helper's identifier for it, by volume and node.
    fn flows(&self) -> impl Future<Output = StoreResult<Vec<Flow>>> + Send;

    /// Every live volume being made from its replica on its node, which has not yet reported it
    /// made.
    fn promoting(&self) -> impl Future<Output = StoreResult<Vec<Promoting>>> + Send;

    /// Each live volume attached to `workload` that was made from a replica, by the name it is
    /// mounted under, with the helper's identifier it was made under.
    fn promoted_ids(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<(String, String)>>> + Send;
}

/// Choosing and dropping replicas.
pub trait ReplicaWrite {
    /// Takes the lock every choice of replicas is made under, until the transaction ends, so
    /// two control-plane replicas never choose for one volume at once.
    fn lock_replicas(&mut self) -> impl Future<Output = StoreResult<()>> + Send;

    /// The replicas of `volume`, by node, in this transaction.
    fn replicas_in(
        &mut self,
        volume: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<Replica>>> + Send;

    /// Records the node of `replica`, a volume and a node, as keeping one of the volume, chosen
    /// at `at`.
    fn choose_replica(
        &mut self,
        replica: (Uuid, Uuid),
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Forgets the replica of `volume` on `node`; whether there was one.
    fn drop_replica(
        &mut self,
        volume: Uuid,
        node: Uuid,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Records a transfer of `snapshot` to `replica`, a volume and a node, under way since `at`.
    fn send_replica(
        &mut self,
        replica: (Uuid, Uuid),
        snapshot: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that `replica`, a volume and a node, received `snapshot` at `at`, if that is the
    /// transfer under way to it; whether it was. It holds it from then, and is no longer stale.
    fn replica_received(
        &mut self,
        replica: (Uuid, Uuid),
        snapshot: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Ends the transfer of `snapshot` to `replica`, a volume and a node, if that is the one
    /// under way to it, so the next pass sends another; whether it was.
    fn replica_failed(
        &mut self,
        replica: (Uuid, Uuid),
        snapshot: Uuid,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Marks `replica`, a volume and a node, stale at `at`, ending any transfer to it.
    fn mark_stale(
        &mut self,
        replica: (Uuid, Uuid),
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Moves the volume of `moved`, a volume and the nodes it leaves and goes to, to the second,
    /// where it is made from a replica of it as `made`, the helper's identifier it takes there,
    /// the replica's snapshot it is made from, and when the replica received it, at `now` (§2.2
    /// of docs/06-storage.md). The copy it leaves is recorded as divergent, with its snapshots;
    /// the replica it is made from is no longer one, its snapshot kept until the node reports
    /// the volume made, and every other is marked stale, to be sent whole. Whether the volume
    /// was on the node it leaves.
    fn promote_replica(
        &mut self,
        moved: (Uuid, Uuid, Uuid),
        made: (&str, Uuid, SystemTime),
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}
