// SPDX-License-Identifier: AGPL-3.0-or-later

//! A volume's replicas (§2.2 of docs/06-storage.md).

use super::StoreResult;
use korpis_core::workload::replica::Replica;
use korpis_core::workload::transfers::Flow;
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
}
