// SPDX-License-Identifier: AGPL-3.0-or-later

//! Receiving. The transfer port hands this node each stream whose ticket is a migration pass's. It
//! is admitted only under a ticket the token keys open, naming this node, a migration whose
//! destination this node is, the peer its role names as the node sending, and a volume and pass its
//! role names, from that peer's mesh address, which WireGuard admits from that node's key alone.
//! **What evaluates "this stream may be written into this volume's copy, from there":** this agent,
//! from the ticket, the keys it holds, and its role, and its kernel, from the mesh's keys; nothing
//! asks the control plane. The pass is received into the destination's copy of the volume by the
//! tree verbs, or into its replica directory by the replica verbs for a volume sent by increments,
//! and the agent journals that it holds it, under the identifier its copy has here.

use super::{Incoming, Migrating, reported};
use crate::host::{Replicating, Trees};
use crate::reconcile::act::volume_id;
use crate::replicas::mesh_of;
use korpis_core::authz::migration_pass::open;
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use korpis_wire::wire_id;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// How long a receive may take to end once its stream has.
const SETTLE: Duration = Duration::from_secs(300);

/// What a stream is admitted into: the assignment, its role, and the volume's part in the pass.
type Admitted = (agent::Assignment, agent::MigrationRole, agent::MigrationVolumeRole);

/// Admits and receives one stream, or refuses it.
pub(super) async fn handle<H: Replicating + Trees>(
    m: Arc<Migrating<H>>,
    (stream, ticket, peer): Incoming,
) {
    match admit(&m, &ticket, peer) {
        Ok((a, role, v)) => {
            let pass = v.pass.as_option().map(|p| p.pass.clone()).unwrap_or_default();
            let mut part = reported(&v, &pass);
            let got = receive(&m, stream, (&a, &v, &pass)).await;
            let failed = match got {
                // One of the volume's own snapshots, which the pass that follows reports.
                Ok(None) => return,
                Ok(Some((helper_id, crossed))) => {
                    (part.held, part.helper_id, part.sent_bytes) = (true, helper_id, crossed);
                    None
                }
                Err(why) => {
                    tracing::warn!(%pass, volume = %v.name, %why, "a pass not received");
                    Some(why)
                }
            };
            // A pass that failed is sent again; only one held is reported.
            if failed.is_none() {
                m.journal(&role, part, None).await;
            }
        }
        Err(why) => tracing::warn!(%peer, %why, "a migration's transfer refused"),
    }
}

/// What the stream with `ticket`, from `peer`, is admitted into.
fn admit<H>(m: &Migrating<H>, ticket: &str, peer: IpAddr) -> Result<Admitted, String> {
    use agent::MigrationSide as S;
    let keys = m.keys.borrow().clone();
    let p = open(&keys, ticket, SystemTime::now()).map_err(|e| format!("the ticket: {e}"))?;
    if p.to != m.node {
        return Err("a ticket naming another node".into());
    }
    let (migration, from) =
        (wire_id(IdKind::Migration, p.migration), wire_id(IdKind::Node, p.from));
    let volume = wire_id(IdKind::Volume, p.volume);
    let pass = p.pass.to_string();
    let held = m.held.borrow();
    let (a, role) = held
        .assignments
        .values()
        .find_map(|a| a.migration.as_option().filter(|r| r.migration == migration).map(|r| (a, r)))
        .ok_or("no migration this node takes part in")?;
    if role.role.as_known() != Some(S::MIGRATION_SIDE_DESTINATION) || role.peer != from {
        return Err("not a migration this node receives from that node".into());
    }
    let v = role
        .volumes
        .iter()
        .find(|v| v.volume == volume && v.pass.as_option().is_some_and(|q| q.pass == pass))
        .ok_or("not the pass under way of a volume of the migration")?;
    if mesh_of(&m.shared().peers, &from).map(IpAddr::V4) != Some(peer) {
        return Err("not from the sending node's mesh address".into());
    }
    Ok((a.clone(), role.clone(), v.clone()))
}

/// The storage class `a`'s target declares the volume named `name` in.
fn class_of(a: &agent::Assignment, name: &str) -> String {
    let body = a.target.as_option().and_then(|t| t.body.as_option());
    let v = body.and_then(|b| b.volumes.iter().find(|v| v.name == name));
    v.map(|v| v.storage_class.clone()).unwrap_or_default()
}

/// Receives what `stream` carries of the pass `pass` of `v`: the pass, or one of the volume's
/// own snapshots sent before it, which is `None`. For the pass, the identifier its copy has here,
/// and, for a tree, the bytes that crossed, which its sender, rsync's daemon, does not count.
async fn receive<H: Replicating + Trees>(
    m: &Migrating<H>,
    mut stream: TcpStream,
    (a, v, pass): (&agent::Assignment, &agent::MigrationVolumeRole, &str),
) -> Result<Option<(String, u64)>, String> {
    let (here, class) = (volume_id(a, &v.name), class_of(a, &v.name));
    let increments =
        v.crossing.as_known() == Some(pb::MigrationCrossing::MIGRATION_CROSSING_INCREMENTS);
    if increments {
        let own = named(&mut stream).await?;
        let request = helper::ReceiveReplicaRequest {
            volume: if own.is_empty() {
                v.source_helper_id.clone()
            } else {
                super::own_snapshots_of(&v.source_helper_id)
            },
            storage_class: class,
            snapshot: if own.is_empty() { pass.to_owned() } else { own.clone() },
            ..Default::default()
        };
        receive_snapshot(m, stream, &request).await?;
        return Ok(own.is_empty().then_some((here, 0)));
    }
    let request = helper::ReceiveTreeRequest {
        storage_class: class,
        volume: here.clone(),
        pass: pass.to_owned(),
        ..Default::default()
    };
    let socket = m.host.receive_tree(&request).await.map_err(|e| e.to_string())?;
    super::join(stream, socket).await?;
    let asked = helper::TreeStatusRequest {
        volume: here.clone(),
        pass: pass.to_owned(),
        sending: false,
        ..Default::default()
    };
    let crossed = super::settled(m, &asked, SETTLE).await?;
    Ok(Some((here, crossed)))
}

/// The name a snapshot's stream begins with, after its ticket: empty for the pass itself.
async fn named(stream: &mut TcpStream) -> Result<String, String> {
    let io = |e: std::io::Error| e.to_string();
    let len = stream.read_u16().await.map_err(io)?;
    let mut name = vec![0u8; usize::from(len)];
    stream.read_exact(&mut name).await.map_err(io)?;
    String::from_utf8(name).map_err(|_| "a snapshot name not UTF-8".to_owned())
}

/// Receives the snapshot `request` names from `stream`, or answers that it is held already.
async fn receive_snapshot<H: Replicating>(
    m: &Migrating<H>,
    mut stream: TcpStream,
    request: &helper::ReceiveReplicaRequest,
) -> Result<(), String> {
    let io = |e: std::io::Error| e.to_string();
    let Some(write) = m.host.receive_replica(request).await.map_err(|e| e.to_string())? else {
        return stream.write_u8(super::HELD).await.map_err(io);
    };
    stream.write_u8(super::RECEIVING).await.map_err(io)?;
    let mut pipe = tokio::fs::File::from_std(std::fs::File::from(write));
    tokio::io::copy(&mut stream, &mut pipe).await.map_err(io)?;
    drop(pipe);
    crate::replicas::receive_settled(&m.host, request).await
}
