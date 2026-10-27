// SPDX-License-Identifier: AGPL-3.0-or-later

//! The replica verbs, kept in memory: a send writes a line naming what it sends into a pipe; a
//! receive reads its pipe to the end on a thread, and holds the snapshot when anything came.

use super::{InMemoryHost, Kept};
use crate::host::{HostError, Replicating};
use korpis_proto::korpis::helper::v1 as helper;
use std::future::{Future, ready};
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

fn io(e: &std::io::Error) -> HostError {
    HostError(format!("a pipe: {e}"))
}

impl InMemoryHost {
    fn send_now(&self, r: &helper::SendReplicaRequest) -> Result<OwnedFd, HostError> {
        let (read, mut write) = std::io::pipe().map_err(|e| io(&e))?;
        let line = format!("replica {} {} from {:?}\n", r.volume, r.snapshot, r.parent);
        write.write_all(line.as_bytes()).map_err(|e| io(&e))?;
        drop(write);
        self.with(|k| {
            k.sent.push(r.clone());
            let sent = k.replicas.entry((String::new(), r.volume.clone())).or_default();
            sent.insert(r.snapshot.clone());
        })?;
        Ok(OwnedFd::from(read))
    }

    fn receive_now(&self, r: &helper::ReceiveReplicaRequest) -> Result<Option<OwnedFd>, HostError> {
        let key = (r.storage_class.clone(), r.volume.clone(), r.snapshot.clone());
        let held = self.with(|k| {
            let at = (key.0.clone(), key.1.clone());
            let held = k.replicas.get(&at).is_some_and(|s| s.contains(&key.2));
            if !held {
                k.receives.insert(key.clone(), helper::ReceiveState::RECEIVE_STATE_RUNNING);
            }
            held
        })?;
        if held {
            return Ok(None);
        }
        let (read, write) = std::io::pipe().map_err(|e| io(&e))?;
        let kept = Arc::clone(&self.kept);
        std::thread::spawn(move || received(&kept, key, read));
        Ok(Some(OwnedFd::from(write)))
    }

    /// The transfer socket `r` asks for, opened here, where the test's addresses are.
    fn transfer_now(r: &helper::OpenTransferRequest) -> Result<OwnedFd, HostError> {
        use rustix::net::{AddressFamily, SocketType, bind, listen, socket};
        let address: std::net::Ipv4Addr =
            r.mesh_address.parse().map_err(|_| HostError("no mesh address".into()))?;
        let port = if r.listen { u16::try_from(r.port).unwrap_or(0) } else { 0 };
        let at = std::net::SocketAddrV4::new(address, port);
        let opened = || -> std::io::Result<OwnedFd> {
            let fd = socket(AddressFamily::INET, SocketType::STREAM, None)?;
            rustix::net::sockopt::set_socket_reuseaddr(&fd, true)?;
            bind(&fd, &rustix::net::SocketAddrAny::from(at))?;
            if r.listen {
                listen(&fd, 64)?;
            }
            Ok(fd)
        };
        opened().map_err(|e| io(&e))
    }

    /// Each stream sent, as it was asked for.
    #[must_use]
    pub fn replicas_sent(&self) -> Vec<helper::SendReplicaRequest> {
        self.with(|k| k.sent.clone()).unwrap_or_default()
    }
}

/// Reads `from` to its end, and holds the snapshot `key` names in `kept` when anything came.
fn received(kept: &Arc<Mutex<Kept>>, key: (String, String, String), mut from: std::io::PipeReader) {
    let mut body = Vec::new();
    let read = from.read_to_end(&mut body);
    let Ok(mut k) = kept.lock() else { return };
    let (class, volume, snapshot) = key.clone();
    let state = if read.is_ok() && !body.is_empty() {
        k.replicas.entry((class, volume)).or_default().insert(snapshot);
        helper::ReceiveState::RECEIVE_STATE_HELD
    } else {
        helper::ReceiveState::RECEIVE_STATE_FAILED
    };
    k.receives.insert(key, state);
}

impl Replicating for InMemoryHost {
    fn send_replica(
        &self,
        r: &helper::SendReplicaRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send {
        ready(self.send_now(r))
    }

    fn receive_replica(
        &self,
        r: &helper::ReceiveReplicaRequest,
    ) -> impl Future<Output = Result<Option<OwnedFd>, HostError>> + Send {
        ready(self.receive_now(r))
    }

    fn receive_status(
        &self,
        r: &helper::ReceiveStatusRequest,
    ) -> impl Future<Output = Result<helper::ReceiveStatusResponse, HostError>> + Send {
        let key = (r.storage_class.clone(), r.volume.clone(), r.snapshot.clone());
        let state = self.with(|k| {
            k.receives.get(&key).copied().unwrap_or(helper::ReceiveState::RECEIVE_STATE_UNKNOWN)
        });
        ready(
            state.map(|s| helper::ReceiveStatusResponse { state: s.into(), ..Default::default() }),
        )
    }

    fn release_replica(
        &self,
        r: &helper::ReleaseReplicaRequest,
    ) -> impl Future<Output = Result<bool, HostError>> + Send {
        ready(self.with(|k| {
            let key = (r.storage_class.clone(), r.volume.clone());
            let released = k.replicas.get_mut(&key).is_some_and(|s| s.remove(&r.snapshot));
            k.replicas.retain(|_, s| !s.is_empty());
            released
        }))
    }

    fn open_transfer(
        &self,
        r: &helper::OpenTransferRequest,
    ) -> impl Future<Output = Result<OwnedFd, HostError>> + Send {
        ready(Self::transfer_now(r))
    }

    fn replica_volumes(
        &self,
    ) -> impl Future<Output = Result<Vec<helper::ReplicaHeld>, HostError>> + Send {
        ready(self.with(|k| {
            k.replicas
                .iter()
                .map(|((class, volume), snapshots)| helper::ReplicaHeld {
                    storage_class: class.clone(),
                    volume: volume.clone(),
                    snapshots: snapshots.iter().cloned().collect(),
                    ..Default::default()
                })
                .collect()
        }))
    }
}
