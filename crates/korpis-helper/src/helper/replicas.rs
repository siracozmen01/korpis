// SPDX-License-Identifier: AGPL-3.0-or-later

//! The replica verbs (§2.2 of docs/06-storage.md): sending from the class holding a volume, and
//! receiving, listing, releasing, and promoting in the class a request names. A receive's
//! process is kept here by its volume and snapshot until a status reads how it ended; a helper
//! that restarted knows of none, and says whether the snapshot is held instead.

use super::Helper;
use crate::error::{HelperError, HelperResult};
use crate::replica::Received;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::helper_request::Verb;
use korpis_proto::korpis::helper::v1::helper_response::Result as Answer;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::os::fd::OwnedFd;

/// The socket `r` asks for: TCP at the node's mesh address, listening on its port or bound to
/// the address alone.
fn transfer_socket(r: &helper::OpenTransferRequest) -> HelperResult<OwnedFd> {
    use rustix::net::{AddressFamily, SocketAddrAny, SocketType, bind, listen, socket};
    let address: Ipv4Addr = r
        .mesh_address
        .parse()
        .ok()
        .filter(|a: &Ipv4Addr| !a.is_unspecified() && !a.is_broadcast() && !a.is_multicast())
        .ok_or_else(|| HelperError::Refused(format!("{:?} is no mesh address", r.mesh_address)))?;
    let port = u16::try_from(r.port)
        .ok()
        .filter(|p| *p >= 1024)
        .ok_or_else(|| HelperError::Refused(format!("{} is no unprivileged port", r.port)))?;
    let at = SocketAddrV4::new(address, if r.listen { port } else { 0 });
    let opened = || -> std::io::Result<OwnedFd> {
        let fd = socket(AddressFamily::INET, SocketType::STREAM, None)?;
        rustix::net::sockopt::set_socket_reuseaddr(&fd, true)?;
        bind(&fd, &SocketAddrAny::from(at))?;
        if r.listen {
            listen(&fd, 64)?;
        }
        Ok(fd)
    };
    Ok(opened()?)
}

impl Helper {
    /// The answer to a replica verb.
    pub(super) fn replica_verb(&mut self, verb: Verb) -> HelperResult<Answer> {
        Ok(match verb {
            Verb::SendReplica(r) => {
                let read =
                    self.holding(&r.volume)?.send_replica(&r.volume, &r.snapshot, &r.parent)?;
                self.attached = vec![read];
                Answer::SendReplica(Box::new(helper::SendReplicaResponse {
                    attached: 1,
                    ..Default::default()
                }))
            }
            Verb::ReceiveReplica(r) => self.receive(&r)?,
            Verb::ReceiveStatus(r) => Answer::ReceiveStatus(Box::new(self.receive_status(&r)?)),
            Verb::ReplicaSnapshots(r) => {
                let mut snapshots = Vec::new();
                for c in self
                    .classes
                    .iter()
                    .filter(|c| r.storage_class.is_empty() || c.name() == r.storage_class)
                {
                    snapshots.extend(c.replica_snapshots(&r.volume)?);
                }
                Answer::ReplicaSnapshots(Box::new(helper::ReplicaSnapshotsResponse {
                    snapshots,
                    ..Default::default()
                }))
            }
            Verb::ReleaseReplica(r) => {
                let released =
                    self.class(&r.storage_class)?.release_replica(&r.volume, &r.snapshot)?;
                Answer::ReleaseReplica(Box::new(helper::ReleaseReplicaResponse {
                    released,
                    ..Default::default()
                }))
            }
            Verb::PromoteReplica(r) => {
                let class = self.class(&r.storage_class)?;
                let path = class.promote_replica(&r.volume, &r.snapshot, &r.into)?;
                Answer::PromoteReplica(Box::new(helper::PromoteReplicaResponse {
                    host_path: path.to_string_lossy().into_owned(),
                    ..Default::default()
                }))
            }
            Verb::ReplicaVolumes(_) => self.replica_volumes()?,
            Verb::OpenTransfer(r) => {
                self.attached = vec![transfer_socket(&r)?];
                let opened = helper::OpenTransferResponse { attached: 1, ..Default::default() };
                Answer::OpenTransfer(Box::new(opened))
            }
            _ => return Err(HelperError::Refused("not a replica verb".into())),
        })
    }

    /// Every volume whose replication keeps a snapshot here, in each class.
    fn replica_volumes(&self) -> HelperResult<Answer> {
        let mut held = Vec::new();
        for c in &self.classes {
            let name = c.name();
            held.extend(c.replica_volumes()?.into_iter().map(|(volume, snapshots)| {
                helper::ReplicaHeld {
                    storage_class: name.to_owned(),
                    volume,
                    snapshots,
                    ..Default::default()
                }
            }));
        }
        Ok(Answer::ReplicaVolumes(Box::new(helper::ReplicaVolumesResponse {
            held,
            ..Default::default()
        })))
    }

    /// Starts a receive, ending one of the same snapshot still running, whose stream a newer
    /// sender replaced.
    fn receive(&mut self, r: &helper::ReceiveReplicaRequest) -> HelperResult<Answer> {
        let key = (r.volume.clone(), r.snapshot.clone());
        if let Some(mut old) = self.receiving.remove(&key) {
            // Ended and reaped, whatever it had read; what it made is removed below.
            let _ = old.kill();
            let _ = old.wait();
        }
        let started = self.class(&r.storage_class)?.receive_replica(&r.volume, &r.snapshot)?;
        let held = started.is_none();
        if let Some((write, child)) = started {
            self.attached = vec![write];
            self.receiving.insert(key, child);
        }
        Ok(Answer::ReceiveReplica(Box::new(helper::ReceiveReplicaResponse {
            attached: u32::from(!held),
            held,
            ..Default::default()
        })))
    }

    /// How the receive `r` names came to, or whether its snapshot is held when none is known.
    fn receive_status(
        &mut self,
        r: &helper::ReceiveStatusRequest,
    ) -> HelperResult<helper::ReceiveStatusResponse> {
        use helper::ReceiveState as S;
        let key = (r.volume.clone(), r.snapshot.clone());
        let (state, detail) = match self.receiving.remove(&key) {
            Some(mut child) => {
                let came =
                    self.class(&r.storage_class)?.received(&r.volume, &r.snapshot, &mut child);
                match came? {
                    Received::Running => {
                        self.receiving.insert(key, child);
                        (S::RECEIVE_STATE_RUNNING, String::new())
                    }
                    Received::Held => (S::RECEIVE_STATE_HELD, String::new()),
                    Received::Failed(why) => (S::RECEIVE_STATE_FAILED, why),
                }
            }
            None if self.class(&r.storage_class)?.holds_replica(&r.volume, &r.snapshot)? => {
                (S::RECEIVE_STATE_HELD, String::new())
            }
            None => (S::RECEIVE_STATE_UNKNOWN, String::new()),
        };
        Ok(helper::ReceiveStatusResponse { state: state.into(), detail, ..Default::default() })
    }
}
