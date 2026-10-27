// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes on the wire (§3.6 of docs/01-model.md): the object a client reads, and what a node
//! reports of one. A size is on the wire only where the write path enforces it (Rule K-3).

use crate::{Result, id, invalid, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::replica::Replica;
use korpis_core::workload::volume::{
    ReportedSnapshot, ReportedVolume, Snapshot, SnapshotEvent, Volume, VolumeEvent, VolumeKind,
};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;

/// The wire form of a volume.
#[must_use]
pub fn volume_to_pb(v: &Volume) -> pb::Volume {
    let kind = match v.kind {
        VolumeKind::Declared => pb::VolumeKind::VOLUME_KIND_DECLARED,
        VolumeKind::Stream => pb::VolumeKind::VOLUME_KIND_STREAM,
    };
    pb::Volume {
        id: wire_id(IdKind::Volume, v.id),
        project: wire_id(IdKind::Project, v.project),
        name: v.name.clone(),
        size_bytes: v.shown_size().unwrap_or(0),
        storage_class: v.storage_class.clone(),
        node: v.node.map(|n| wire_id(IdKind::Node, n)),
        store: v.store.clone().unwrap_or_default(),
        attached_to: v.attached_to.map(|w| wire_id(IdKind::Workload, w)),
        mode: pb::VolumeMode::VOLUME_MODE_RW.into(),
        created: timestamp(v.created).into(),
        deleted_at: v.deleted_at.map(timestamp).into(),
        quota_enforced: v.quota_enforced,
        kind: kind.into(),
        detached_at: v.detached_at.map(timestamp).into(),
        created_by: wire_id(IdKind::Workload, v.created_by),
        snapshots: v.snapshots.iter().map(|s| wire_id(IdKind::Snapshot, *s)).collect(),
        replicas: v.replicas.iter().map(replica_to_pb).collect(),
        ..Default::default()
    }
}

/// The wire form of a replica.
fn replica_to_pb(r: &Replica) -> pb::VolumeReplica {
    pb::VolumeReplica {
        node: wire_id(IdKind::Node, r.node),
        chosen_at: timestamp(r.chosen_at).into(),
        received_at: r.held.map(|(_, at)| timestamp(at)).into(),
        stale_at: r.stale_at.map(timestamp).into(),
        ..Default::default()
    }
}

/// The wire form of a snapshot.
#[must_use]
pub fn snapshot_to_pb(s: &Snapshot) -> pb::Snapshot {
    pb::Snapshot {
        id: wire_id(IdKind::Snapshot, s.id),
        volume: wire_id(IdKind::Volume, s.volume),
        name: s.name.clone(),
        created: timestamp(s.created).into(),
        deleted_at: s.deleted_at.map(timestamp).into(),
        ..Default::default()
    }
}

/// What a node reports of a snapshot, from the field `field`. The node names a snapshot by the
/// identifier the plan gave it, in its bare form, as the helper knows it.
pub fn snapshot_from_pb(field: &str, e: &agent::SnapshotEvent) -> Result<ReportedSnapshot> {
    let event = match e.event.as_known() {
        Some(agent::SnapshotEventKind::SNAPSHOT_EVENT_KIND_TAKEN) => SnapshotEvent::Taken,
        Some(agent::SnapshotEventKind::SNAPSHOT_EVENT_KIND_ROLLED_BACK) => {
            SnapshotEvent::RolledBack
        }
        Some(agent::SnapshotEventKind::SNAPSHOT_EVENT_KIND_DELETED) => SnapshotEvent::Deleted,
        _ => return Err(invalid(&format!("{field}.event"), "taken, rolled back, or deleted")),
    };
    let snapshot = uuid::Uuid::parse_str(&e.snapshot)
        .map_err(|_| invalid(&format!("{field}.snapshot"), "the snapshot's identifier"))?;
    if e.volume.is_empty() {
        return Err(invalid(&format!("{field}.volume"), "the helper's identifier of the volume"));
    }
    Ok(ReportedSnapshot {
        workload: id(IdKind::Workload, &format!("{field}.workload"), &e.workload)?,
        volume: e.volume.clone(),
        snapshot,
        name: e.name.clone(),
        event,
    })
}

/// What a node reports of a volume, from the field `field`.
pub fn reported_from_pb(field: &str, e: &agent::VolumeEvent) -> Result<ReportedVolume> {
    let event = match e.event.as_known() {
        Some(agent::VolumeEventKind::VOLUME_EVENT_KIND_ENSURED) => VolumeEvent::Ensured,
        Some(agent::VolumeEventKind::VOLUME_EVENT_KIND_DELETED) => VolumeEvent::Deleted,
        _ => return Err(invalid(&format!("{field}.event"), "ensured or deleted")),
    };
    if e.name.is_empty() || e.helper_id.is_empty() {
        return Err(invalid(field, "a volume reported has a name and a helper identifier"));
    }
    Ok(ReportedVolume {
        workload: id(IdKind::Workload, &format!("{field}.workload"), &e.workload)?,
        name: e.name.clone(),
        helper_id: e.helper_id.clone(),
        storage_class: e.storage_class.clone(),
        size_bytes: e.size_bytes,
        quota_enforced: e.quota_enforced,
        event,
        kind: if e.stream { VolumeKind::Stream } else { VolumeKind::Declared },
    })
}
