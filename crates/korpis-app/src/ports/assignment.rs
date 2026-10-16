// SPDX-License-Identifier: AGPL-3.0-or-later

//! Assignments (§4.8 of docs/02-architecture.md): sealed by the control plane's assignment key,
//! kept with their generation, and acknowledged by the node.

use super::StoreResult;
use korpis_core::workload::assignment::Assignment;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// An assignment as it travels and is kept: its exact bytes and the signature over them, so a
/// node can prove what it was told (§8 of docs/03-state.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// The encoded assignment.
    pub body: Vec<u8>,
    /// The signature over `body`.
    pub signature: Vec<u8>,
    /// The key that made it.
    pub key_id: String,
}

/// Why an assignment could not be sealed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("sealing an assignment: {0}")]
pub struct SealError(pub String);

/// Encodes and signs assignments: the wire format and the key are the adapter's.
pub trait AssignmentSealer: Send + Sync + std::fmt::Debug {
    /// A digest of the assignment with its generation ignored: equal exactly when nothing a
    /// node is told has changed.
    fn content_digest(&self, assignment: &Assignment) -> Result<Vec<u8>, SealError>;

    /// The assignment's bytes and signature.
    fn seal(&self, assignment: &Assignment) -> Result<Sealed, SealError>;
}

/// An assignment as the store keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAssignment {
    /// The workload.
    pub workload: Uuid,
    /// Its generation on this node.
    pub generation: u64,
    /// Over its content, generation ignored.
    pub content_digest: Vec<u8>,
    /// What was sent.
    pub sealed: Sealed,
    /// Whether it is in the node's set; an inactive one is kept for its generation.
    pub active: bool,
}

/// Reading what assignments are computed from.
pub trait AssignmentRead {
    /// The workloads placed on `node`.
    fn placed_on(&self, node: Uuid) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// The digest of the set `node` last acknowledged holding, if it acknowledged one.
    fn acked_set(&self, node: Uuid) -> impl Future<Output = StoreResult<Option<String>>> + Send;

    /// The workloads not placed on `node` with an approved, queued, or applying plan deleting a
    /// retained volume there: those whose residual assignment it is sent (§7 of
    /// docs/06-storage.md).
    fn residual_on(&self, node: Uuid) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// The nodes a change to `workload` concerns: those it is placed on, those holding an active
    /// assignment for it, and those holding a volume an approved plan of it deletes.
    fn nodes_of(&self, workload: Uuid) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// The epoch of the lease `node` holds for `workload`, if it holds one.
    fn lease_epoch(
        &self,
        workload: Uuid,
        node: Uuid,
    ) -> impl Future<Output = StoreResult<Option<u64>>> + Send;

    /// Whether `node` has ever held a lease for `workload`, released or not: what a node must have
    /// had to run it, and so to have written anything of it that outlives its stay.
    fn ever_leased(
        &self,
        workload: Uuid,
        node: Uuid,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}

/// Writing assignments.
pub trait AssignmentWrite {
    /// Serializes every computation of `node`'s set until the transaction ends.
    fn lock_assignments(&mut self, node: Uuid) -> impl Future<Output = StoreResult<()>> + Send;

    /// Every assignment `node` has ever had, active or not.
    fn stored_assignments(
        &mut self,
        node: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<StoredAssignment>>> + Send;

    /// Writes an assignment of `node`, active.
    fn put_assignment(
        &mut self,
        node: Uuid,
        assignment: &StoredAssignment,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// `workload`'s id slot on `node`: the one it holds, or the lowest free below `slots`,
    /// after giving back the slots of retired workloads; `None` if every slot is held.
    fn id_slot(
        &mut self,
        node: Uuid,
        workload: Uuid,
        slots: u32,
    ) -> impl Future<Output = StoreResult<Option<u32>>> + Send;

    /// Takes `workload`'s assignment out of `node`'s set, keeping its generation.
    fn deactivate_assignment(
        &mut self,
        node: Uuid,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records the set digest `node` acknowledged at `at`.
    fn record_ack(
        &mut self,
        node: Uuid,
        digest: &str,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
