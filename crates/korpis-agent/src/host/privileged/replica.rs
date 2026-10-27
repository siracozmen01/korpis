// SPDX-License-Identifier: AGPL-3.0-or-later

//! The replica verbs (§2.2 of docs/06-storage.md): a send's stream and a receive's are pipes
//! whose ends the helper's answer carries.

use super::{HelperClient, failed};
use crate::host::HostError;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;
use std::os::fd::OwnedFd;

impl HelperClient {
    /// Takes the snapshot `r` names of a volume here and starts sending it; the pipe's read end.
    pub async fn send_replica(&self, r: &helper::SendReplicaRequest) -> Result<OwnedFd, HostError> {
        match self.call_attached(Verb::SendReplica(Box::new(r.clone()))).await? {
            (Answer::SendReplica(a), mut fds) if a.attached == 1 && fds.len() == 1 => {
                fds.pop().ok_or_else(|| failed("no pipe"))
            }
            (other, fds) => {
                Err(failed(format!("answered {other:?} with {} descriptors", fds.len())))
            }
        }
    }

    /// Starts receiving the snapshot `r` names into a replica here; the pipe's write end, or
    /// none when it is held already.
    pub async fn receive_replica(
        &self,
        r: &helper::ReceiveReplicaRequest,
    ) -> Result<Option<OwnedFd>, HostError> {
        match self.call_attached(Verb::ReceiveReplica(Box::new(r.clone()))).await? {
            (Answer::ReceiveReplica(a), fds) if a.held && fds.is_empty() => Ok(None),
            (Answer::ReceiveReplica(a), mut fds) if a.attached == 1 && fds.len() == 1 => {
                fds.pop().map(Some).ok_or_else(|| failed("no pipe"))
            }
            (other, fds) => {
                Err(failed(format!("answered {other:?} with {} descriptors", fds.len())))
            }
        }
    }

    /// How the receive `r` names came to.
    pub async fn receive_status(
        &self,
        r: &helper::ReceiveStatusRequest,
    ) -> Result<helper::ReceiveStatusResponse, HostError> {
        match self.call(Verb::ReceiveStatus(Box::new(r.clone()))).await? {
            Answer::ReceiveStatus(a) => Ok(*a),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }

    /// Deletes the snapshot `r` names of a volume's replication here; whether it was here.
    pub async fn release_replica(
        &self,
        r: &helper::ReleaseReplicaRequest,
    ) -> Result<bool, HostError> {
        match self.call(Verb::ReleaseReplica(Box::new(r.clone()))).await? {
            Answer::ReleaseReplica(a) => Ok(a.released),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }

    /// Makes the volume `r` names a writable copy of its replica's snapshot; its path.
    pub async fn promote_replica(
        &self,
        r: &helper::PromoteReplicaRequest,
    ) -> Result<String, HostError> {
        match self.call(Verb::PromoteReplica(Box::new(r.clone()))).await? {
            Answer::PromoteReplica(a) => Ok(a.host_path),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }

    /// The transfer socket `r` asks for, at the node's mesh address.
    pub async fn open_transfer(
        &self,
        r: &helper::OpenTransferRequest,
    ) -> Result<OwnedFd, HostError> {
        match self.call_attached(Verb::OpenTransfer(Box::new(r.clone()))).await? {
            (Answer::OpenTransfer(a), mut fds) if a.attached == 1 && fds.len() == 1 => {
                fds.pop().ok_or_else(|| failed("no socket"))
            }
            (other, fds) => {
                Err(failed(format!("answered {other:?} with {} descriptors", fds.len())))
            }
        }
    }

    /// Every volume whose replication keeps a snapshot here, in each class.
    pub async fn replica_volumes(&self) -> Result<Vec<helper::ReplicaHeld>, HostError> {
        match self.call(Verb::ReplicaVolumes(Box::default())).await? {
            Answer::ReplicaVolumes(a) => Ok(a.held),
            other => Err(failed(format!("answered {other:?}"))),
        }
    }
}
