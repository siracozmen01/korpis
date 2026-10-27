// SPDX-License-Identifier: AGPL-3.0-or-later

//! Receiving (§2.2 of docs/06-storage.md). The node listens on its own mesh address, which its
//! table admits from the mesh only for the nodes it was told send it replicas. A stream is
//! admitted only under a ticket the token keys open, naming this node, a replica it was told it
//! receives, the snapshot it was told comes, and the node it was told sends it, from that node's
//! mesh address, which WireGuard admits from that node's key alone. **What evaluates "this
//! stream may be written into this replica, from there":** this agent, from the ticket, the keys
//! it holds, and what it was told, and its kernel, from the mesh's keys; nothing asks the control
//! plane. The helper receives what was admitted, and the agent journals how it ended.

use super::{RETRY, Replication, mesh_of};
use crate::host::Replicating;
use korpis_core::authz::transfer::open;
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::helper::v1 as helper;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::io::AsyncReadExt as _;
use tokio::net::{TcpListener, TcpStream};

/// The longest ticket read: one is far shorter.
const MAX_TICKET: u16 = 512;
/// How long the ticket may take to arrive.
const HEADER: Duration = Duration::from_secs(10);
/// How long a receive may take to end once its stream has.
const SETTLE: Duration = Duration::from_secs(300);

/// The socket the node receives on at `mesh`, opened by the helper where the mesh is.
async fn listener<H: Replicating>(
    r: &Replication<H>,
    mesh: Ipv4Addr,
) -> Result<TcpListener, String> {
    let request = helper::OpenTransferRequest {
        mesh_address: mesh.to_string(),
        port: u32::from(super::TRANSFER_PORT),
        listen: true,
        ..Default::default()
    };
    let fd = r.host.open_transfer(&request).await.map_err(|e| e.to_string())?;
    let listener = std::net::TcpListener::from(fd);
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    TcpListener::from_std(listener).map_err(|e| e.to_string())
}

/// Listens on `mesh` for the rest of the agent's life, asking for the socket again every
/// [`RETRY`] until the address is on the interface.
pub(super) async fn listen<H: Replicating + Clone>(r: Arc<Replication<H>>, mesh: Ipv4Addr) {
    loop {
        match listener(&r, mesh).await {
            Ok(listener) => {
                tracing::info!(%mesh, "receiving replicas");
                while let Ok((stream, peer)) = listener.accept().await {
                    tokio::spawn(handle(Arc::clone(&r), stream, peer));
                }
            }
            Err(e) => tracing::debug!(error = %e, "the transfer port is not open yet"),
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// Admits and receives one stream, or refuses it.
async fn handle<H: Replicating>(r: Arc<Replication<H>>, mut stream: TcpStream, peer: SocketAddr) {
    match admit(&r, &mut stream, peer.ip()).await {
        Ok((kept, snapshot)) => {
            let event = receive(&r, stream, &kept, &snapshot).await;
            let body = Body::Replica(Box::new(event));
            if let Err(e) = r.journal.append(body).await {
                tracing::error!(error = %e, "a receive could not be journaled");
            }
        }
        Err(why) => tracing::warn!(%peer, %why, "a transfer refused"),
    }
}

/// The replica a stream from `peer` is admitted into and the snapshot it carries, once its
/// ticket is read and checked.
async fn admit<H: Replicating>(
    r: &Replication<H>,
    stream: &mut TcpStream,
    peer: IpAddr,
) -> Result<(agent::ReplicaKept, String), String> {
    let read = async {
        let len = stream.read_u16().await?;
        if len > MAX_TICKET {
            return Err(std::io::Error::other("a ticket too long"));
        }
        let mut ticket = vec![0u8; usize::from(len)];
        stream.read_exact(&mut ticket).await?;
        Ok(ticket)
    };
    let ticket = tokio::time::timeout(HEADER, read)
        .await
        .map_err(|_| "no ticket in time".to_owned())?
        .map_err(|e| e.to_string())?;
    let ticket = String::from_utf8(ticket).map_err(|_| "a ticket that is not text")?;
    let keys = r.keys.borrow().clone();
    let t = open(&keys, &ticket, SystemTime::now()).map_err(|e| format!("the ticket: {e}"))?;
    if t.to != r.node {
        return Err("a ticket naming another node".into());
    }
    let (volume, from) = (
        korpis_wire::wire_id(IdKind::Volume, t.volume),
        korpis_wire::wire_id(IdKind::Node, t.from),
    );
    let snapshot = t.snapshot.to_string();
    let shared = r.shared();
    let told = shared.told.as_ref().ok_or("told nothing of replicas yet")?;
    let kept = told
        .keeping
        .iter()
        .find(|k| k.volume == volume && k.from == from && k.receiving == snapshot)
        .ok_or("not a snapshot this node was told it receives, from that node")?;
    if mesh_of(&shared.peers, &from).map(IpAddr::V4) != Some(peer) {
        return Err("not from the sending node's mesh address".into());
    }
    Ok((kept.clone(), snapshot))
}

/// Receives what `stream` carries into the replica `kept`, as snapshot `snapshot`; how it ended.
async fn receive<H: Replicating>(
    r: &Replication<H>,
    mut stream: TcpStream,
    kept: &agent::ReplicaKept,
    snapshot: &str,
) -> agent::ReplicaEvent {
    let request = helper::ReceiveReplicaRequest {
        volume: kept.volume.clone(),
        storage_class: kept.storage_class.clone(),
        snapshot: snapshot.to_owned(),
        ..Default::default()
    };
    let ended = match r.host.receive_replica(&request).await {
        Ok(None) => Ok(()),
        Ok(Some(write)) => {
            let mut pipe = tokio::fs::File::from_std(std::fs::File::from(write));
            let copied = tokio::io::copy(&mut stream, &mut pipe).await;
            drop(pipe);
            match copied {
                Ok(_) => settled(r, &request).await,
                Err(e) => Err(format!("the stream: {e}")),
            }
        }
        Err(e) => Err(e.to_string()),
    };
    let (event, error) = match ended {
        Ok(()) => (agent::ReplicaEventKind::REPLICA_EVENT_KIND_RECEIVED, String::new()),
        Err(why) => (agent::ReplicaEventKind::REPLICA_EVENT_KIND_FAILED, why),
    };
    agent::ReplicaEvent {
        volume: kept.volume.clone(),
        snapshot: snapshot.to_owned(),
        event: event.into(),
        error,
        ..Default::default()
    }
}

/// Waits for the receive `request` started to end, its stream having ended; why it failed.
async fn settled<H: Replicating>(
    r: &Replication<H>,
    request: &helper::ReceiveReplicaRequest,
) -> Result<(), String> {
    use helper::ReceiveState as S;
    let asked = helper::ReceiveStatusRequest {
        volume: request.volume.clone(),
        storage_class: request.storage_class.clone(),
        snapshot: request.snapshot.clone(),
        ..Default::default()
    };
    let deadline = tokio::time::Instant::now() + SETTLE;
    while tokio::time::Instant::now() < deadline {
        let status = r.host.receive_status(&asked).await.map_err(|e| e.to_string())?;
        match status.state.as_known() {
            Some(S::RECEIVE_STATE_HELD) => return Ok(()),
            Some(S::RECEIVE_STATE_FAILED) => return Err(status.detail),
            Some(S::RECEIVE_STATE_UNKNOWN) => return Err("the helper lost the receive".into()),
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
    Err("the receive did not end in time".into())
}
