// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the agent asks of the helper for replicas (§2.2 of docs/06-storage.md): sending a
//! volume's snapshot, receiving one into a replica, reading how a receive ended, releasing what
//! replication no longer needs, and making a volume of a replica.

use super::HostError;
use super::node::NodeHost;
use korpis_proto::korpis::helper::v1 as helper;
use std::future::Future;
use std::os::fd::OwnedFd;

/// The helper's replica verbs.
pub trait Replicating: Send + Sync + 'static {
    /// Takes the snapshot `r` names of a volume here and starts sending it, whole or from the
    /// parent it names; the read end of the pipe the stream is written into.
    fn send_replica(
        &self,
        r: &helper::SendReplicaRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send;

    /// Starts receiving the snapshot `r` names into a replica here; the write end of the pipe
    /// the stream is read from, or none when the snapshot is held already.
    fn receive_replica(
        &self,
        r: &helper::ReceiveReplicaRequest,
    ) -> impl Future<Output = Result<Option<OwnedFd>, HostError>> + Send;

    /// How the receive `r` names came to.
    fn receive_status(
        &self,
        r: &helper::ReceiveStatusRequest,
    ) -> impl Future<Output = Result<helper::ReceiveStatusResponse, HostError>> + Send;

    /// Deletes the snapshot `r` names of a volume's replication here; whether it was here.
    fn release_replica(
        &self,
        r: &helper::ReleaseReplicaRequest,
    ) -> impl Future<Output = Result<bool, HostError>> + Send;

    /// Every volume whose replication keeps a snapshot here, in each class.
    fn replica_volumes(
        &self,
    ) -> impl Future<Output = Result<Vec<helper::ReplicaHeld>, HostError>> + Send;

    /// Makes the volume `r` names here a writable copy of a snapshot its replica here holds;
    /// the volume's path.
    fn promote_replica(
        &self,
        r: &helper::PromoteReplicaRequest,
    ) -> impl Future<Output = Result<String, HostError>> + Send;

    /// A TCP socket at the node's mesh address, opened where the mesh is: listening on the
    /// port `r` names, or bound to the address alone for sending from.
    fn open_transfer(
        &self,
        r: &helper::OpenTransferRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send;
}

impl Replicating for NodeHost {
    fn send_replica(
        &self,
        r: &helper::SendReplicaRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send {
        self.helper().send_replica(r)
    }

    fn receive_replica(
        &self,
        r: &helper::ReceiveReplicaRequest,
    ) -> impl Future<Output = Result<Option<OwnedFd>, HostError>> + Send {
        self.helper().receive_replica(r)
    }

    fn receive_status(
        &self,
        r: &helper::ReceiveStatusRequest,
    ) -> impl Future<Output = Result<helper::ReceiveStatusResponse, HostError>> + Send {
        self.helper().receive_status(r)
    }

    fn release_replica(
        &self,
        r: &helper::ReleaseReplicaRequest,
    ) -> impl Future<Output = Result<bool, HostError>> + Send {
        self.helper().release_replica(r)
    }

    fn replica_volumes(
        &self,
    ) -> impl Future<Output = Result<Vec<helper::ReplicaHeld>, HostError>> + Send {
        self.helper().replica_volumes()
    }

    fn promote_replica(
        &self,
        r: &helper::PromoteReplicaRequest,
    ) -> impl Future<Output = Result<String, HostError>> + Send {
        self.helper().promote_replica(r)
    }

    fn open_transfer(
        &self,
        r: &helper::OpenTransferRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send {
        self.helper().open_transfer(r)
    }
}
