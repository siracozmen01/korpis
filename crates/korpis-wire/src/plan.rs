// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plans, their steps, and their impact. Plans are computed by the control plane and leave it;
//! the one way back in is a restore recovering a plan a node presents (§8 of docs/03-state.md),
//! which [`read`] serves.

mod read;

use read::{executor_to_pb, layer_to_pb, status_to_pb, survival_to_pb};
pub use read::{plan_from_pb, step_from_pb};

use crate::spec::endpoints::exposure_to_pb;
use crate::value::map_to_pb;
use crate::{duration, principal, scope, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::endpoint::ExposureImpact;
use korpis_core::workload::plan::VolumeOp;
use korpis_core::workload::plan::{Approval, Estimate, Impact, ResourceDelta, Step};
use korpis_core::workload::{Plan, PlanTarget};
use korpis_proto::korpis::v1 as pb;

/// The wire form of a step.
#[must_use]
pub fn step_to_pb(s: &Step) -> pb::Step {
    pb::Step {
        index: s.index,
        action: s.action.as_str().to_owned(),
        executor: executor_to_pb(s.executor).into(),
        target: pb::ObjectRef {
            kind: crate::scope::object_kind_to_pb(s.target.kind).into(),
            id: wire_id(crate::id_kind(s.target.kind), s.target.id),
            ..Default::default()
        }
        .into(),
        reversible: s.reversible,
        disruptive: s.disruptive,
        detail: map_to_pb(&s.detail),
        timeout: s.timeout.map(duration).into(),
        classified_by: layer_to_pb(s.classified_by).into(),
        reason: s.reason.clone(),
        ..Default::default()
    }
}

fn delta_to_pb(d: ResourceDelta) -> pb::ResourceDelta {
    pb::ResourceDelta {
        cpu_millis: d.cpu_millis,
        memory_bytes: d.memory_bytes,
        disk_bytes: d.disk_bytes,
        volumes: d.volumes,
        endpoints: d.endpoints,
        workloads: d.workloads,
        ..Default::default()
    }
}

/// The wire form of an impact, with the reason the plan does or does not apply on its own.
#[must_use]
pub fn impact_to_pb(i: &Impact, approval_reason: &str) -> pb::Impact {
    pb::Impact {
        disruptive: i.disruptive,
        downtime_estimate: match &i.downtime {
            Estimate::Known(d) => Some(duration(*d)),
            Estimate::Unknown => None,
        }
        .into(),
        downtime_unknown: !i.downtime.is_known(),
        data_at_risk: match &i.data_at_risk {
            Estimate::Known(v) => v.clone(),
            Estimate::Unknown => Vec::new(),
        },
        data_at_risk_unknown: !i.data_at_risk.is_known(),
        approval_reason: approval_reason.to_owned(),
        cost_delta: delta_to_pb(i.cost_delta).into(),
        exposures: i.exposures.iter().map(exposure_impact_to_pb).collect(),
        ..Default::default()
    }
}

fn exposure_impact_to_pb(e: &ExposureImpact) -> pb::ExposureImpact {
    pb::ExposureImpact {
        endpoint: e.endpoint.clone(),
        declared: exposure_to_pb(e.declared).into(),
        resolved: exposure_to_pb(e.resolved).into(),
        survives: survival_to_pb(e.survives).into(),
        measured_inside: e.measured_inside,
        ..Default::default()
    }
}

fn approval(a: &Approval) -> (pb::PlanApproval, Option<pb::Scope>) {
    match a {
        Approval::Auto => (pb::PlanApproval::PLAN_APPROVAL_AUTO, None),
        Approval::Confirmation => (pb::PlanApproval::PLAN_APPROVAL_CONFIRMATION, None),
        Approval::Authority { scope: s } => {
            (pb::PlanApproval::PLAN_APPROVAL_AUTHORITY, Some(scope::to_pb(s)))
        }
    }
}

fn volume_kind(op: VolumeOp) -> pb::PlanKind {
    match op {
        VolumeOp::Delete => pb::PlanKind::PLAN_KIND_DELETE_VOLUME,
        VolumeOp::Snapshot => pb::PlanKind::PLAN_KIND_SNAPSHOT_VOLUME,
        VolumeOp::Rollback => pb::PlanKind::PLAN_KIND_ROLLBACK_VOLUME,
        VolumeOp::DeleteSnapshot => pb::PlanKind::PLAN_KIND_DELETE_SNAPSHOT,
        VolumeOp::Backup => pb::PlanKind::PLAN_KIND_BACKUP_VOLUME,
        VolumeOp::Restore => pb::PlanKind::PLAN_KIND_RESTORE_VOLUME,
    }
}

/// The wire form of a plan.
#[must_use]
pub fn plan_to_pb(p: &Plan) -> pb::Plan {
    let (kind, to_intent, to_power) = match p.target {
        PlanTarget::Intent(i) => (pb::PlanKind::PLAN_KIND_INTENT, Some(i), None),
        PlanTarget::Power(v) => (pb::PlanKind::PLAN_KIND_POWER, None, Some(v)),
        PlanTarget::Retire => (pb::PlanKind::PLAN_KIND_RETIRE, None, None),
        PlanTarget::Migrate => (pb::PlanKind::PLAN_KIND_MIGRATE, None, None),
        PlanTarget::Remake => (pb::PlanKind::PLAN_KIND_REMAKE, None, None),
        PlanTarget::Volume(_, op) => (volume_kind(op), None, None),
        PlanTarget::Prune(_) => (pb::PlanKind::PLAN_KIND_PRUNE_BACKUPS, None, None),
    };
    let to_volume = p.target.volume().map(|v| wire_id(IdKind::Volume, v));
    let to_repository = p.target.repository().map(|r| wire_id(IdKind::Repository, r));
    let (approval, approval_scope) = approval(&p.approval);
    pb::Plan {
        id: wire_id(IdKind::Plan, p.id),
        workload: wire_id(IdKind::Workload, p.workload),
        kind: kind.into(),
        operation: p.operation.map(|o| wire_id(IdKind::Operation, o)),
        from_intent: p.from_intent.map(|i| wire_id(IdKind::Intent, i)),
        from_power: p.from_power,
        to_intent: to_intent.map(|i| wire_id(IdKind::Intent, i)),
        to_power,
        computed_at: timestamp(p.computed_at).into(),
        computed_by: principal::to_pb(&p.computed_by).into(),
        steps: p.steps.iter().map(step_to_pb).collect(),
        impact: impact_to_pb(&p.impact, &p.approval_reason).into(),
        status: status_to_pb(p.status).into(),
        completed: p.completed,
        apply_after: p.apply_after.map(timestamp).into(),
        expires_at: timestamp(p.expires_at).into(),
        approved_by: p.approved_by.as_ref().map(principal::to_pb).into(),
        approved_at: p.approved_at.map(timestamp).into(),
        proposed_under: p.proposed_under.map(|g| wire_id(IdKind::Grant, g)).unwrap_or_default(),
        approval: approval.into(),
        approval_scope: approval_scope.into(),
        to_volume,
        to_repository,
        ..Default::default()
    }
}
