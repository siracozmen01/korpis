// SPDX-License-Identifier: AGPL-3.0-or-later

//! Replicas between nodes on the wire (§2.2 of docs/06-storage.md): what a node is told of them,
//! and what it reports of a receive. Snapshots are named by their bare identifiers, as the
//! helper knows them.

use crate::{Result, id, invalid, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::replica::{ReplicaEvent, ReportedReplica};
use korpis_core::workload::transfers::NodeReplicas;
use korpis_proto::korpis::agent::v1 as agent;
use uuid::Uuid;

/// The longest reason for a failed receive kept: the rest is the node's to log.
const MAX_ERROR: usize = 1024;

/// What a node reports of a receive into a replica it keeps, from the field `field`.
pub fn reported_from_pb(field: &str, e: &agent::ReplicaEvent) -> Result<ReportedReplica> {
    let event = match e.event.as_known() {
        Some(agent::ReplicaEventKind::REPLICA_EVENT_KIND_RECEIVED) => ReplicaEvent::Received,
        Some(agent::ReplicaEventKind::REPLICA_EVENT_KIND_FAILED) => {
            ReplicaEvent::Failed(e.error.chars().take(MAX_ERROR).collect())
        }
        _ => return Err(invalid(&format!("{field}.event"), "received or failed")),
    };
    let snapshot = Uuid::parse_str(&e.snapshot)
        .map_err(|_| invalid(&format!("{field}.snapshot"), "the snapshot's identifier"))?;
    Ok(ReportedReplica {
        volume: id(IdKind::Volume, &format!("{field}.volume"), &e.volume)?,
        snapshot,
        event,
    })
}

/// The wire form of what a node is told of replicas.
#[must_use]
pub fn replicas_to_pb(told: &NodeReplicas) -> agent::Replicas {
    let bare = |s: Option<Uuid>| s.map(|s| s.to_string()).unwrap_or_default();
    agent::Replicas {
        sending: told
            .sending
            .iter()
            .map(|o| agent::ReplicaSource {
                volume: wire_id(IdKind::Volume, o.volume),
                helper_id: o.helper_id.clone(),
                keep: o.keep.iter().map(Uuid::to_string).collect(),
                transfers: o
                    .transfers
                    .iter()
                    .map(|t| agent::ReplicaTransfer {
                        to: wire_id(IdKind::Node, t.to),
                        snapshot: t.snapshot.to_string(),
                        parent: bare(t.parent),
                        expires_at: timestamp(t.expires_at).into(),
                        ticket: t.ticket.clone(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            })
            .collect(),
        keeping: told
            .keeping
            .iter()
            .map(|k| agent::ReplicaKept {
                volume: wire_id(IdKind::Volume, k.volume),
                holds: bare(k.holds),
                receiving: bare(k.receiving),
                from: wire_id(IdKind::Node, k.from),
                storage_class: k.storage_class.clone(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}
