// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plans read back: the one place a plan enters the control plane from outside, a restore
//! recovering the plan a node presents in full inside a signed assignment (§8 of
//! docs/03-state.md). The inverse of [`super::plan_to_pb`].

use crate::spec::endpoints::exposure_from_pb;
use crate::value::map_from_pb;
use crate::{Result, duration_from, id, id_kind, invalid, principal, scope, time};
use korpis_core::id::IdKind;
use korpis_core::object::ObjectRef;
use korpis_core::workload::endpoint::{ExposureImpact, Survival};
use korpis_core::workload::plan::{
    Approval, Estimate, Impact, Layer, PlanStatus, ResourceDelta, Step, StepAction, StepExecutor,
    VolumeOp,
};
use korpis_core::workload::{Plan, PlanTarget};
use korpis_proto::korpis::v1 as pb;

enum_map!(status_to_pb, status_from_pb, PlanStatus, pb::PlanStatus, "a plan status", {
    PlanStatus::Proposed => pb::PlanStatus::PLAN_STATUS_PROPOSED,
    PlanStatus::Approved => pb::PlanStatus::PLAN_STATUS_APPROVED,
    PlanStatus::Queued => pb::PlanStatus::PLAN_STATUS_QUEUED,
    PlanStatus::Applying => pb::PlanStatus::PLAN_STATUS_APPLYING,
    PlanStatus::Applied => pb::PlanStatus::PLAN_STATUS_APPLIED,
    PlanStatus::Failed => pb::PlanStatus::PLAN_STATUS_FAILED,
    PlanStatus::Cancelled => pb::PlanStatus::PLAN_STATUS_CANCELLED,
    PlanStatus::Rejected => pb::PlanStatus::PLAN_STATUS_REJECTED,
    PlanStatus::Expired => pb::PlanStatus::PLAN_STATUS_EXPIRED,
    PlanStatus::Superseded => pb::PlanStatus::PLAN_STATUS_SUPERSEDED,
});

enum_map!(layer_to_pb, layer_from_pb, Layer, pb::StepLayer, "a classification layer", {
    Layer::Vocabulary => pb::StepLayer::STEP_LAYER_VOCABULARY,
    Layer::Driver => pb::StepLayer::STEP_LAYER_DRIVER,
    Layer::Recipe => pb::StepLayer::STEP_LAYER_RECIPE,
    Layer::Operator => pb::StepLayer::STEP_LAYER_OPERATOR,
});

enum_map!(executor_to_pb, executor_from_pb, StepExecutor, pb::StepExecutor, "a step executor", {
    StepExecutor::ControlPlane => pb::StepExecutor::STEP_EXECUTOR_CONTROL_PLANE,
    StepExecutor::Node => pb::StepExecutor::STEP_EXECUTOR_NODE,
    StepExecutor::Extension => pb::StepExecutor::STEP_EXECUTOR_EXTENSION,
});

enum_map!(survival_to_pb, survival_from_pb, Survival, pb::ExposureSurvival, "a survival", {
    Survival::NoMove => pb::ExposureSurvival::EXPOSURE_SURVIVAL_NO_MOVE,
    Survival::WhileItsNodeExists => pb::ExposureSurvival::EXPOSURE_SURVIVAL_WHILE_ITS_NODE_EXISTS,
    Survival::Moves => pb::ExposureSurvival::EXPOSURE_SURVIVAL_MOVES,
    Survival::Unreachable => pb::ExposureSurvival::EXPOSURE_SURVIVAL_UNREACHABLE,
});

/// A step from the field `field`.
pub fn step_from_pb(field: &str, s: &pb::Step) -> Result<Step> {
    let f = |name: &str| format!("{field}.{name}");
    let target = s.target.as_option().ok_or_else(|| invalid(&f("target"), "required"))?;
    let kind = crate::scope::object_kind_from_pb(&f("target.kind"), target.kind)?;
    Ok(Step {
        index: s.index,
        action: StepAction::parse(&s.action)
            .ok_or_else(|| invalid(&f("action"), "not an action of the vocabulary"))?,
        executor: executor_from_pb(&f("executor"), s.executor)?,
        target: ObjectRef { kind, id: id(id_kind(kind), &f("target.id"), &target.id)? },
        reversible: s.reversible,
        disruptive: s.disruptive,
        classified_by: layer_from_pb(&f("classified_by"), s.classified_by)?,
        reason: s.reason.clone(),
        detail: map_from_pb(&f("detail"), &s.detail)?,
        timeout: s.timeout.as_option().map(|d| duration_from(&f("timeout"), d)).transpose()?,
    })
}

fn delta_from_pb(d: Option<&pb::ResourceDelta>) -> ResourceDelta {
    d.map_or_else(ResourceDelta::default, |d| ResourceDelta {
        cpu_millis: d.cpu_millis,
        memory_bytes: d.memory_bytes,
        disk_bytes: d.disk_bytes,
        volumes: d.volumes,
        endpoints: d.endpoints,
        workloads: d.workloads,
    })
}

fn impact_from_pb(field: &str, i: &pb::Impact) -> Result<Impact> {
    let downtime = match i.downtime_estimate.as_option() {
        Some(d) if !i.downtime_unknown => Estimate::Known(duration_from(field, d)?),
        _ => Estimate::Unknown,
    };
    let exposures = i
        .exposures
        .iter()
        .map(|e| {
            Ok(ExposureImpact {
                endpoint: e.endpoint.clone(),
                declared: exposure_from_pb(&format!("{field}.exposures"), e.declared)?,
                resolved: exposure_from_pb(&format!("{field}.exposures"), e.resolved)?,
                survives: survival_from_pb(&format!("{field}.exposures"), e.survives)?,
                measured_inside: e.measured_inside,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Impact {
        disruptive: i.disruptive,
        downtime,
        data_at_risk: if i.data_at_risk_unknown {
            Estimate::Unknown
        } else {
            Estimate::Known(i.data_at_risk.clone())
        },
        cost_delta: delta_from_pb(i.cost_delta.as_option()),
        exposures,
    })
}

fn approval_from_pb(p: &pb::Plan) -> Result<Approval> {
    match p.approval.as_known() {
        Some(pb::PlanApproval::PLAN_APPROVAL_AUTO) => Ok(Approval::Auto),
        Some(pb::PlanApproval::PLAN_APPROVAL_CONFIRMATION) => Ok(Approval::Confirmation),
        Some(pb::PlanApproval::PLAN_APPROVAL_AUTHORITY) => {
            let s = p.approval_scope.as_option();
            let s = s.ok_or_else(|| invalid("approval_scope", "required for authority"))?;
            Ok(Approval::Authority { scope: scope::from_pb("approval_scope", s)? })
        }
        _ => Err(invalid("approval", "expected an approval")),
    }
}

fn target_from_pb(p: &pb::Plan) -> Result<PlanTarget> {
    let volume = || {
        let v = p.to_volume.as_deref().ok_or_else(|| invalid("to_volume", "required"))?;
        id(IdKind::Volume, "to_volume", v)
    };
    let op = |op| Ok(PlanTarget::Volume(volume()?, op));
    match p.kind.as_known() {
        Some(pb::PlanKind::PLAN_KIND_INTENT) => {
            let i = p.to_intent.as_deref().ok_or_else(|| invalid("to_intent", "required"))?;
            Ok(PlanTarget::Intent(id(IdKind::Intent, "to_intent", i)?))
        }
        Some(pb::PlanKind::PLAN_KIND_POWER) => {
            Ok(PlanTarget::Power(p.to_power.ok_or_else(|| invalid("to_power", "required"))?))
        }
        Some(pb::PlanKind::PLAN_KIND_RETIRE) => Ok(PlanTarget::Retire),
        Some(pb::PlanKind::PLAN_KIND_MIGRATE) => Ok(PlanTarget::Migrate),
        Some(pb::PlanKind::PLAN_KIND_REMAKE) => Ok(PlanTarget::Remake),
        Some(pb::PlanKind::PLAN_KIND_DELETE_VOLUME) => op(VolumeOp::Delete),
        Some(pb::PlanKind::PLAN_KIND_SNAPSHOT_VOLUME) => op(VolumeOp::Snapshot),
        Some(pb::PlanKind::PLAN_KIND_ROLLBACK_VOLUME) => op(VolumeOp::Rollback),
        Some(pb::PlanKind::PLAN_KIND_DELETE_SNAPSHOT) => op(VolumeOp::DeleteSnapshot),
        Some(pb::PlanKind::PLAN_KIND_BACKUP_VOLUME) => op(VolumeOp::Backup),
        Some(pb::PlanKind::PLAN_KIND_RESTORE_VOLUME) => op(VolumeOp::Restore),
        Some(pb::PlanKind::PLAN_KIND_PRUNE_BACKUPS) => {
            let r = p.to_repository.as_deref();
            let r = r.ok_or_else(|| invalid("to_repository", "required"))?;
            Ok(PlanTarget::Prune(id(IdKind::Repository, "to_repository", r)?))
        }
        _ => Err(invalid("kind", "expected a plan kind")),
    }
}

/// A plan, as [`super::plan_to_pb`] wrote it.
pub fn plan_from_pb(p: &pb::Plan) -> Result<Plan> {
    let required = |field: &str| invalid(field, "required");
    let at = |field: &str, t: Option<&buffa_types::google::protobuf::Timestamp>| -> Result<_> {
        time(field, t.ok_or_else(|| required(field))?)
    };
    let impact = p.impact.as_option().ok_or_else(|| required("impact"))?;
    let by = p.computed_by.as_option().ok_or_else(|| required("computed_by"))?;
    Ok(Plan {
        id: id(IdKind::Plan, "id", &p.id)?,
        workload: id(IdKind::Workload, "workload", &p.workload)?,
        target: target_from_pb(p)?,
        operation: p
            .operation
            .as_deref()
            .map(|o| id(IdKind::Operation, "operation", o))
            .transpose()?,
        from_intent: p
            .from_intent
            .as_deref()
            .map(|i| id(IdKind::Intent, "from_intent", i))
            .transpose()?,
        from_power: p.from_power,
        computed_at: at("computed_at", p.computed_at.as_option())?,
        computed_by: principal::from_pb("computed_by", by)?,
        proposed_under: (!p.proposed_under.is_empty())
            .then(|| id(IdKind::Grant, "proposed_under", &p.proposed_under))
            .transpose()?,
        steps: p
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| step_from_pb(&format!("steps[{i}]"), s))
            .collect::<Result<_>>()?,
        impact: impact_from_pb("impact", impact)?,
        approval: approval_from_pb(p)?,
        approval_reason: impact.approval_reason.clone(),
        status: status_from_pb("status", p.status)?,
        completed: p.completed,
        apply_after: p.apply_after.as_option().map(|t| time("apply_after", t)).transpose()?,
        expires_at: at("expires_at", p.expires_at.as_option())?,
        approved_by: p
            .approved_by
            .as_option()
            .map(|s| principal::from_pb("approved_by", s))
            .transpose()?,
        approved_at: p.approved_at.as_option().map(|t| time("approved_at", t)).transpose()?,
    })
}
