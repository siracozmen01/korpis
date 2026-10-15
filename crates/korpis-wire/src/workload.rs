// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workloads, intents, and power declarations.

use crate::spec::body_to_pb;
use crate::{Result, invalid, principal, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::plan::{Override, StepAction};
use korpis_core::workload::set::{DeclaredSet, DriftMode};
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
        power_target: w.power_target_version,
        name: w.name.as_str().to_owned(),
        labels: w.labels.as_map().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        intent: wire_id(IdKind::Intent, w.intent),
        target: w.target.map(|t| wire_id(IdKind::Intent, t)),
        power: power_to_pb(power).into(),
        created: timestamp(w.created).into(),
        deleted_at: w.deleted_at.map(timestamp).into(),
        set: w.set.map(|s| wire_id(IdKind::Set, s)),
        classification: w
            .classification
            .iter()
            .map(|o| pb::ClassificationOverride {
                action: o.action.as_str().to_owned(),
                disruptive: o.disruptive,
                ..Default::default()
            })
            .collect(),
        version: w.version,
        unreproducible: w
            .unreproducible
            .as_ref()
            .map(|u| pb::Unreproducible {
                at: timestamp(u.at).into(),
                reason: u.reason.clone(),
                ..Default::default()
            })
            .into(),
        ..Default::default()
    }
}

/// An operator's classification overrides. An action outside the vocabulary is refused; the
/// rules on the set itself are the domain's (`Override::check`).
pub fn classification_from_pb(
    field: &str,
    overrides: &[pb::ClassificationOverride],
) -> Result<Vec<Override>> {
    overrides
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let action = StepAction::parse(&o.action)
                .ok_or_else(|| invalid(&format!("{field}[{i}].action"), "not a step action"))?;
            Ok(Override { action, disruptive: o.disruptive })
        })
        .collect()
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
        managed_by: i.managed_by.map(|s| wire_id(IdKind::Set, s)),
        ..Default::default()
    }
}

/// The wire form of a drift mode.
#[must_use]
pub fn drift_mode_to_pb(m: DriftMode) -> pb::DriftMode {
    match m {
        DriftMode::Advisory => pb::DriftMode::DRIFT_MODE_ADVISORY,
        DriftMode::Strict => pb::DriftMode::DRIFT_MODE_STRICT,
    }
}

/// The drift mode a request names.
///
/// # Errors
///
/// When it names none.
pub fn drift_mode_from_pb(field: &str, m: buffa::EnumValue<pb::DriftMode>) -> Result<DriftMode> {
    match m.as_known() {
        Some(pb::DriftMode::DRIFT_MODE_ADVISORY) => Ok(DriftMode::Advisory),
        Some(pb::DriftMode::DRIFT_MODE_STRICT) => Ok(DriftMode::Strict),
        _ => Err(invalid(field, "advisory or strict")),
    }
}

/// The wire form of a declared set.
#[must_use]
pub fn declared_set_to_pb(s: &DeclaredSet) -> pb::DeclaredSet {
    pb::DeclaredSet {
        id: wire_id(IdKind::Set, s.id),
        project: wire_id(IdKind::Project, s.project),
        name: s.name.as_str().to_owned(),
        mode: drift_mode_to_pb(s.mode).into(),
        version: s.version,
        created_by: principal::to_pb(&s.created_by).into(),
        created_at: timestamp(s.created_at).into(),
        ..Default::default()
    }
}

/// The wire form of a convergence.
#[must_use]
pub fn convergence_to_pb(c: korpis_core::workload::status::Convergence) -> pb::Convergence {
    use korpis_core::workload::status::Convergence as C;
    match c {
        C::Converged => pb::Convergence::CONVERGENCE_CONVERGED,
        C::Applying => pb::Convergence::CONVERGENCE_APPLYING,
        C::AwaitingApproval => pb::Convergence::CONVERGENCE_AWAITING_APPROVAL,
        C::Scheduled => pb::Convergence::CONVERGENCE_SCHEDULED,
        C::Failing => pb::Convergence::CONVERGENCE_FAILING,
        C::Unknown => pb::Convergence::CONVERGENCE_UNKNOWN,
        C::Unplaced => pb::Convergence::CONVERGENCE_UNPLACED,
    }
}
