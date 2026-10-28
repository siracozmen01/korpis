// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sending (§8.1 of docs/05-scheduling.md). For each volume of a migration whose source this node
//! is, in `replicate` or `final_sync`, the pass its role names is sent once: from this node's mesh
//! address to the destination's transfer port, the ticket first, then the pass, by increments
//! where the volume crosses so and by its tree otherwise; then journaled, with the bytes it sent
//! for increments; a tree's are counted by the destination, whose rsync pulls it.
//! One that failed is sent again after [`RETRY`]; a pass the control plane started anew, its
//! ticket having run out, is another pass.

use super::{Migrating, RETRY, Sending, reported};
use crate::host::{Replicating, Trees};
use crate::replicas::{TRANSFER_PORT, mesh_of};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::time::Instant;

/// How long connecting may take.
const CONNECT: Duration = Duration::from_secs(10);
/// How long a tree's sender may take to end once its stream has.
const SETTLE: Duration = Duration::from_secs(300);

/// A pass due to be sent: the role, the volume's part in it, and the pass.
type Due = (agent::MigrationRole, agent::MigrationVolumeRole, agent::MigrationPass);

/// Every pass this node's roles name to be sent now.
fn due<H>(m: &Migrating<H>) -> Vec<Due> {
    use agent::MigrationPhase as P;
    use agent::MigrationSide as S;
    let held = m.held.borrow();
    let copying = |r: &agent::MigrationRole| {
        r.role.as_known() == Some(S::MIGRATION_SIDE_SOURCE)
            && matches!(
                r.phase.as_known(),
                Some(P::MIGRATION_PHASE_REPLICATE | P::MIGRATION_PHASE_FINAL_SYNC)
            )
    };
    let mut out = Vec::new();
    for role in held.assignments.values().filter_map(|a| a.migration.as_option()) {
        if !copying(role) {
            continue;
        }
        for v in &role.volumes {
            if let Some(p) = v.pass.as_option() {
                out.push((role.clone(), v.clone(), p.clone()));
            }
        }
    }
    out
}

/// Starts sending each pass due that is not sent or being sent, or that failed long enough ago.
pub(super) fn start_due<H: Replicating + Trees + Clone>(m: &Arc<Migrating<H>>) {
    let now = Instant::now();
    for (role, volume, pass) in due(m) {
        {
            let mut shared = m.shared();
            let ready = match shared.sends.get(&pass.pass) {
                None => true,
                Some(Sending::Failed(at)) => *at + RETRY <= now,
                Some(Sending::Running | Sending::Sent) => false,
            };
            if !ready {
                continue;
            }
            shared.sends.insert(pass.pass.clone(), Sending::Running);
        }
        tokio::spawn(sending(Arc::clone(m), (role, volume, pass)));
    }
}

/// Sends one pass, records how it went, and journals what it sent.
async fn sending<H: Replicating + Trees>(m: Arc<Migrating<H>>, (role, v, pass): Due) {
    let to = mesh_of(&m.shared().peers, &role.peer);
    let sent = send(&m, (&v, &pass), to).await;
    let state = match &sent {
        Ok(_) => Sending::Sent,
        Err(why) => {
            tracing::warn!(pass = %pass.pass, volume = %v.name, %why, "a pass not sent");
            Sending::Failed(Instant::now())
        }
    };
    m.shared().sends.insert(pass.pass.clone(), state);
    if let Ok(bytes) = sent {
        let mut part = reported(&v, &pass.pass);
        part.sent_bytes = bytes;
        m.journal(&role, part, None).await;
    }
}

/// The connection to `to`'s transfer port from this node's mesh address.
async fn connect<H: Replicating>(m: &Migrating<H>, to: Ipv4Addr) -> Result<TcpStream, String> {
    let from = m.mesh.ok_or("this node has no mesh address")?;
    let request = helper::OpenTransferRequest {
        mesh_address: from.to_string(),
        port: u32::from(TRANSFER_PORT),
        listen: false,
        ..Default::default()
    };
    let fd = m.host.open_transfer(&request).await.map_err(|e| e.to_string())?;
    let unconnected = std::net::TcpStream::from(fd);
    unconnected.set_nonblocking(true).map_err(|e| e.to_string())?;
    let socket = tokio::net::TcpSocket::from_std_stream(unconnected);
    let connect = socket.connect(SocketAddr::from((to, TRANSFER_PORT)));
    tokio::time::timeout(CONNECT, connect)
        .await
        .map_err(|_| "connecting timed out".to_owned())?
        .map_err(|e| e.to_string())
}

/// Sends `pass` of the volume `v` to `to`; the bytes it sent.
async fn send<H: Replicating + Trees>(
    m: &Migrating<H>,
    (v, pass): (&agent::MigrationVolumeRole, &agent::MigrationPass),
    to: Option<Ipv4Addr>,
) -> Result<u64, String> {
    let to = to.ok_or("the destination's mesh address is not known")?;
    let ticket = pass.ticket.as_bytes();
    let len = u16::try_from(ticket.len()).map_err(|_| "a ticket too long")?;
    let mut stream = connect(m, to).await?;
    let io = |e: std::io::Error| e.to_string();
    stream.write_all(&len.to_be_bytes()).await.map_err(io)?;
    stream.write_all(ticket).await.map_err(io)?;
    let increments =
        v.crossing.as_known() == Some(pb::MigrationCrossing::MIGRATION_CROSSING_INCREMENTS);
    if increments {
        let request = helper::SendReplicaRequest {
            volume: v.source_helper_id.clone(),
            snapshot: pass.pass.clone(),
            parent: pass.parent.clone(),
            ..Default::default()
        };
        let read = m.host.send_replica(&request).await.map_err(|e| e.to_string())?;
        let mut pipe = tokio::fs::File::from_std(std::fs::File::from(read));
        let bytes = tokio::io::copy(&mut pipe, &mut stream).await.map_err(io)?;
        stream.shutdown().await.map_err(io)?;
        // The destination closes once its receive ended; what it says is in its journal.
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest).await;
        return Ok(bytes);
    }
    let request = helper::SendTreeRequest {
        volume: v.source_helper_id.clone(),
        pass: pass.pass.clone(),
        ..Default::default()
    };
    let socket = m.host.send_tree(&request).await.map_err(|e| e.to_string())?;
    super::join(stream, socket).await?;
    let asked = helper::TreeStatusRequest {
        volume: request.volume,
        pass: request.pass,
        sending: true,
        ..Default::default()
    };
    super::settled(m, &asked, SETTLE).await
}
