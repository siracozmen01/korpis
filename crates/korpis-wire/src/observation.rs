// SPDX-License-Identifier: AGPL-3.0-or-later

//! Observations: `korpis.v1.Observation` (§4.3 of docs/02-architecture.md).

use crate::{Result, id, invalid, time, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::observation::{
    EndpointSeen, Health, Observation, ObservedState, UnsatisfiableReason, Usage, VolumeSeen,
};
use korpis_proto::korpis::v1 as pb;
use uuid::Uuid;

enum_map!(state_to_pb, state_from_pb, ObservedState, pb::ObservedState, "an observed state", {
    ObservedState::Running => pb::ObservedState::OBSERVED_STATE_RUNNING,
    ObservedState::Stopped => pb::ObservedState::OBSERVED_STATE_STOPPED,
    ObservedState::Starting => pb::ObservedState::OBSERVED_STATE_STARTING,
    ObservedState::Stopping => pb::ObservedState::OBSERVED_STATE_STOPPING,
    ObservedState::Crashed => pb::ObservedState::OBSERVED_STATE_CRASHED,
    ObservedState::Unsatisfiable => pb::ObservedState::OBSERVED_STATE_UNSATISFIABLE,
});

enum_map!(reason_to_pb, reason_from_pb, UnsatisfiableReason, pb::UnsatisfiableReason,
    "an unsatisfiable reason", {
    UnsatisfiableReason::MissingDriver => pb::UnsatisfiableReason::UNSATISFIABLE_REASON_MISSING_DRIVER,
    UnsatisfiableReason::InsufficientResources =>
        pb::UnsatisfiableReason::UNSATISFIABLE_REASON_INSUFFICIENT_RESOURCES,
    UnsatisfiableReason::VolumeUnavailable =>
        pb::UnsatisfiableReason::UNSATISFIABLE_REASON_VOLUME_UNAVAILABLE,
    UnsatisfiableReason::DeviceUnavailable =>
        pb::UnsatisfiableReason::UNSATISFIABLE_REASON_DEVICE_UNAVAILABLE,
    UnsatisfiableReason::ImageUnsupported =>
        pb::UnsatisfiableReason::UNSATISFIABLE_REASON_IMAGE_UNSUPPORTED,
    UnsatisfiableReason::PrepareRejected =>
        pb::UnsatisfiableReason::UNSATISFIABLE_REASON_PREPARE_REJECTED,
});

enum_map!(health_to_pb, health_from_pb, Health, pb::HealthState, "a health state", {
    Health::Healthy => pb::HealthState::HEALTH_STATE_HEALTHY,
    Health::Unhealthy => pb::HealthState::HEALTH_STATE_UNHEALTHY,
    Health::Unknown => pb::HealthState::HEALTH_STATE_UNKNOWN,
});

fn usage_to_pb(u: Usage) -> pb::ResourceUsage {
    pb::ResourceUsage {
        cpu_millis: u.cpu_millis,
        memory_bytes: u.memory_bytes,
        disk_bytes: u.disk_bytes,
        disk_inodes: u.disk_inodes,
        pids: u.pids,
        net_rx_bytes: u.net_rx_bytes,
        net_tx_bytes: u.net_tx_bytes,
        ..Default::default()
    }
}

fn endpoint_to_pb(e: &EndpointSeen) -> pb::EndpointStatus {
    pb::EndpointStatus {
        name: e.name.clone(),
        address: e.address.clone(),
        ready: e.ready,
        ..Default::default()
    }
}

fn volume_to_pb(v: &VolumeSeen) -> pb::VolumeStatus {
    pb::VolumeStatus {
        name: v.name.clone(),
        volume: v.volume.clone(),
        used_bytes: v.used_bytes,
        used_inodes: v.used_inodes,
        mounted: v.mounted,
        ..Default::default()
    }
}

fn endpoint_from_pb(e: &pb::EndpointStatus) -> EndpointSeen {
    EndpointSeen { name: e.name.clone(), address: e.address.clone(), ready: e.ready }
}

fn volume_from_pb(v: &pb::VolumeStatus) -> VolumeSeen {
    VolumeSeen {
        name: v.name.clone(),
        volume: v.volume.clone(),
        used_bytes: v.used_bytes,
        used_inodes: v.used_inodes,
        mounted: v.mounted,
    }
}

/// The wire form of an observation. One that is not current says `UNKNOWN` as its state, so
/// a remembered value is never shown as current (P4); its node is named only if `show_node`.
#[must_use]
pub fn observation_to_pb(o: &Observation, current: bool, show_node: bool) -> pb::Observation {
    let state =
        if current { state_to_pb(o.state) } else { pb::ObservedState::OBSERVED_STATE_UNKNOWN };
    pb::Observation {
        workload: wire_id(IdKind::Workload, o.workload),
        node: if show_node { wire_id(IdKind::Node, o.node) } else { String::new() },
        observed_at: timestamp(o.observed_at).into(),
        assignment_seen: o.assignment_seen,
        intent_seen: o.intent_seen.map(|i| wire_id(IdKind::Intent, i)).unwrap_or_default(),
        power_seen: pb::PowerSeen {
            version: o.power_seen,
            cycle: o.cycle_seen,
            ..Default::default()
        }
        .into(),
        plan_progress: o
            .plan_progress
            .map(|(plan, step)| pb::PlanProgress {
                plan: wire_id(IdKind::Plan, plan),
                step,
                ..Default::default()
            })
            .into(),
        state: state.into(),
        reason: o
            .reason
            .map_or(pb::UnsatisfiableReason::UNSATISFIABLE_REASON_UNSPECIFIED, reason_to_pb)
            .into(),
        reason_detail: o.reason_detail.clone(),
        health: health_to_pb(o.health).into(),
        exit_code: o.exit_code,
        runtime_id: o.runtime_id.clone(),
        resources: usage_to_pb(o.usage).into(),
        endpoints: o.endpoints.iter().map(endpoint_to_pb).collect(),
        volumes: o.volumes.iter().map(volume_to_pb).collect(),
        since: o.since.map(timestamp).into(),
        address: o.address.map(|a| a.to_string()).unwrap_or_default(),
        ..Default::default()
    }
}

fn usage(r: Option<&pb::ResourceUsage>) -> Usage {
    r.map_or_else(Usage::default, |r| Usage {
        cpu_millis: r.cpu_millis,
        memory_bytes: r.memory_bytes,
        disk_bytes: r.disk_bytes,
        disk_inodes: r.disk_inodes,
        pids: r.pids,
        net_rx_bytes: r.net_rx_bytes,
        net_tx_bytes: r.net_tx_bytes,
    })
}

/// An observation `node` sent.
pub fn observation_from_pb(node: Uuid, o: &pb::Observation) -> Result<Observation> {
    let f = |name: &str| format!("observation.{name}");
    if !o.node.is_empty() && id(IdKind::Node, &f("node"), &o.node)? != node {
        return Err(invalid(&f("node"), "a node reports for itself"));
    }
    let observed_at =
        o.observed_at.as_option().ok_or_else(|| invalid(&f("observed_at"), "required"))?;
    let reason = match o.reason.as_known() {
        None | Some(pb::UnsatisfiableReason::UNSATISFIABLE_REASON_UNSPECIFIED) => None,
        Some(_) => Some(reason_from_pb(&f("reason"), o.reason)?),
    };
    let intent_seen = (!o.intent_seen.is_empty())
        .then(|| id(IdKind::Intent, &f("intent_seen"), &o.intent_seen))
        .transpose()?;
    let plan_progress = o
        .plan_progress
        .as_option()
        .map(|p| {
            Ok::<_, crate::WireError>((
                id(IdKind::Plan, &f("plan_progress.plan"), &p.plan)?,
                p.step,
            ))
        })
        .transpose()?;
    let power = o.power_seen.as_option();
    Ok(Observation {
        workload: id(IdKind::Workload, &f("workload"), &o.workload)?,
        node,
        observed_at: time(&f("observed_at"), observed_at)?,
        assignment_seen: o.assignment_seen,
        intent_seen,
        power_seen: power.map_or(0, |p| p.version),
        cycle_seen: power.map_or(0, |p| p.cycle),
        plan_progress,
        state: state_from_pb(&f("state"), o.state)?,
        reason,
        reason_detail: o.reason_detail.clone(),
        health: health_from_pb(&f("health"), o.health).unwrap_or(Health::Unknown),
        exit_code: o.exit_code,
        runtime_id: o.runtime_id.clone(),
        usage: usage(o.resources.as_option()),
        endpoints: o.endpoints.iter().map(endpoint_from_pb).collect(),
        volumes: o.volumes.iter().map(volume_from_pb).collect(),
        since: o.since.as_option().map(|t| time(&f("since"), t)).transpose()?,
        address: (!o.address.is_empty())
            .then(|| o.address.parse())
            .transpose()
            .map_err(|_| invalid(&f("address"), "an IPv4 address"))?,
    })
}
