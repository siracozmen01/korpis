// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workloads, intents, and power declarations.

use crate::spec::body_to_pb;
use crate::{Result, invalid, principal, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::{Intent, PowerRecord, PowerState, Workload};
use korpis_proto::korpis::v1 as pb;

/// The wire form of a power declaration.
#[must_use]
pub fn power_to_pb(p: &PowerRecord) -> pb::Power {
    pb::Power {
        workload: wire_id(IdKind::Workload, p.workload),
        desired: match p.power.desired {
            PowerState::Running => pb::PowerState::POWER_STATE_RUNNING,
            PowerState::Stopped => pb::PowerState::POWER_STATE_STOPPED,
        }
        .into(),
        cycle: p.power.cycle,
        version: p.power.version,
        set_by: principal::to_pb(&p.set_by).into(),
        set_at: timestamp(p.set_at).into(),
        ..Default::default()
    }
}

/// A requested power state from the field `field`; unspecified means running.
pub fn power_state_from_pb(field: &str, s: buffa::EnumValue<pb::PowerState>) -> Result<PowerState> {
    match s.as_known() {
        Some(pb::PowerState::POWER_STATE_UNSPECIFIED | pb::PowerState::POWER_STATE_RUNNING) => {
            Ok(PowerState::Running)
        }
        Some(pb::PowerState::POWER_STATE_STOPPED) => Ok(PowerState::Stopped),
        None => Err(invalid(field, "unknown power state")),
    }
}

/// The wire form of a workload with its current power declaration.
#[must_use]
pub fn workload_to_pb(w: &Workload, power: &PowerRecord) -> pb::Workload {
    pb::Workload {
        id: wire_id(IdKind::Workload, w.id),
        project: wire_id(IdKind::Project, w.project),
        organization: wire_id(IdKind::Organization, w.organization),
        name: w.name.as_str().to_owned(),
        labels: w.labels.as_map().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        intent: wire_id(IdKind::Intent, w.intent),
        target: w.target.map(|t| wire_id(IdKind::Intent, t)),
        power: power_to_pb(power).into(),
        created: timestamp(w.created).into(),
        deleted_at: w.deleted_at.map(timestamp).into(),
        ..Default::default()
    }
}

/// The wire form of an intent.
#[must_use]
pub fn intent_to_pb(i: &Intent) -> pb::Intent {
    pb::Intent {
        id: wire_id(IdKind::Intent, i.id),
        workload: wire_id(IdKind::Workload, i.workload),
        version: i.version,
        parent: i.parent.map(|p| wire_id(IdKind::Intent, p)),
        declared_by: principal::to_pb(&i.declared_by).into(),
        declared_at: timestamp(i.declared_at).into(),
        body: body_to_pb(&i.body).into(),
        ..Default::default()
    }
}
