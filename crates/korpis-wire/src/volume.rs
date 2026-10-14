// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes on the wire (§3.6 of docs/01-model.md): the object a client reads, and what a node
//! reports of one. A size is on the wire only where the write path enforces it (Rule K-3).

use crate::{Result, id, invalid, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::volume::{ReportedVolume, Volume, VolumeEvent, VolumeKind};
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
        node: Some(wire_id(IdKind::Node, v.node)),
        attached_to: v.attached_to.map(|w| wire_id(IdKind::Workload, w)),
        mode: pb::VolumeMode::VOLUME_MODE_RW.into(),
        created: timestamp(v.created).into(),
        deleted_at: v.deleted_at.map(timestamp).into(),
        quota_enforced: v.quota_enforced,
        kind: kind.into(),
        detached_at: v.detached_at.map(timestamp).into(),
        created_by: wire_id(IdKind::Workload, v.created_by),
        ..Default::default()
    }
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
