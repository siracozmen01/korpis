// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sending (§2.2 of docs/06-storage.md): each transfer the node is told of, from its own mesh
//! address to the receiving node's, the ticket first, a two-byte big-endian length and then its
//! text, and then the helper's stream to its end. A transfer is sent once; one that failed is
//! sent again [`RETRY`] later while its ticket lasts, and the receiving node journals how it
//! ended. A transfer the node is no longer told of is forgotten.

use super::{RETRY, Replication, SendKey, Sending, TRANSFER_PORT, mesh_of};
use crate::host::Replicating;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::time::Instant;

/// How long connecting to the receiving node may take.
const CONNECT: Duration = Duration::from_secs(10);

/// When `t` stops working.
fn expires(t: &agent::ReplicaTransfer) -> SystemTime {
    let seconds = t.expires_at.as_option().map_or(0, |e| u64::try_from(e.seconds).unwrap_or(0));
    UNIX_EPOCH + Duration::from_secs(seconds)
}

/// Starts each transfer the node is told of that is due: not yet sent, or failed [`RETRY`] ago,
/// and not expired.
pub(super) fn start_due<H: Replicating + Clone>(r: &Arc<Replication<H>>) {
    let (now, at) = (Instant::now(), SystemTime::now());
    let mut shared = r.shared();
    let Some(told) = shared.told.clone() else { return };
    let transfers: Vec<(&agent::ReplicaSource, &agent::ReplicaTransfer)> =
        told.sending.iter().flat_map(|s| s.transfers.iter().map(move |t| (s, t))).collect();
    let told_of: BTreeSet<SendKey> = transfers.iter().map(|(s, t)| key(s, t)).collect();
    shared.sends.retain(|k, _| told_of.contains(k));
    for (source, t) in transfers {
        let k = key(source, t);
        let due = match shared.sends.get(&k) {
            None => true,
            Some(Sending::Failed(when)) => *when + RETRY <= now,
            Some(Sending::Running | Sending::Sent) => false,
        };
        if !due || expires(t) <= at {
            continue;
        }
        shared.sends.insert(k.clone(), Sending::Running);
        let to = mesh_of(&shared.peers, &t.to);
        let job = (source.helper_id.clone(), t.clone(), to);
        tokio::spawn(sent(Arc::clone(r), k, job));
    }
}

fn key(s: &agent::ReplicaSource, t: &agent::ReplicaTransfer) -> SendKey {
    (s.volume.clone(), t.to.clone(), t.snapshot.clone())
}

/// Sends one transfer, and records how it stands after.
async fn sent<H: Replicating>(
    r: Arc<Replication<H>>,
    k: SendKey,
    (helper_id, t, to): (String, agent::ReplicaTransfer, Option<Ipv4Addr>),
) {
    let state = match send(&r, &helper_id, &t, to).await {
        Ok(()) => {
            tracing::info!(volume = %k.0, to = %k.1, snapshot = %k.2, "replica sent");
            Sending::Sent
        }
        Err(why) => {
            tracing::warn!(volume = %k.0, to = %k.1, snapshot = %k.2, %why, "replica not sent");
            Sending::Failed(Instant::now())
        }
    };
    let mut shared = r.shared();
    if let Some(held) = shared.sends.get_mut(&k) {
        *held = state;
    }
}

/// Sends `t` of the volume the helper knows as `helper_id` to `to`; why not, if it was not
/// written whole.
async fn send<H: Replicating>(
    r: &Replication<H>,
    helper_id: &str,
    t: &agent::ReplicaTransfer,
    to: Option<Ipv4Addr>,
) -> Result<(), String> {
    let to = to.ok_or("the receiving node's mesh address is not known")?;
    let from = r.mesh.ok_or("this node has no mesh address")?;
    let ticket = t.ticket.as_bytes();
    let len = u16::try_from(ticket.len()).map_err(|_| "a ticket too long")?;
    let request = helper::SendReplicaRequest {
        volume: helper_id.to_owned(),
        snapshot: t.snapshot.clone(),
        parent: t.parent.clone(),
        ..Default::default()
    };
    let read = r.host.send_replica(&request).await.map_err(|e| e.to_string())?;
    let io = |e: std::io::Error| e.to_string();
    let request = helper::OpenTransferRequest {
        mesh_address: from.to_string(),
        port: u32::from(TRANSFER_PORT),
        listen: false,
        ..Default::default()
    };
    let fd = r.host.open_transfer(&request).await.map_err(|e| e.to_string())?;
    let unconnected = std::net::TcpStream::from(fd);
    unconnected.set_nonblocking(true).map_err(io)?;
    let socket = tokio::net::TcpSocket::from_std_stream(unconnected);
    let connect = socket.connect(SocketAddr::from((to, TRANSFER_PORT)));
    let mut stream = tokio::time::timeout(CONNECT, connect)
        .await
        .map_err(|_| "connecting timed out".to_owned())?
        .map_err(io)?;
    stream.write_all(&len.to_be_bytes()).await.map_err(io)?;
    stream.write_all(ticket).await.map_err(io)?;
    let mut pipe = tokio::fs::File::from_std(std::fs::File::from(read));
    tokio::io::copy(&mut pipe, &mut stream).await.map_err(io)?;
    stream.shutdown().await.map_err(io)?;
    // The receiving node closes once its receive ended; what it says is in its journal.
    let mut rest = Vec::new();
    let _ = stream.read_to_end(&mut rest).await;
    Ok(())
}
