// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plans, their steps, and their impact. Plans are computed by the control plane and only ever
//! leave it, so there is no reading direction.

use crate::value::map_to_pb;
use crate::{duration, principal, scope, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::plan::{
    Approval, Estimate, Impact, Layer, PlanStatus, ResourceDelta, Step, StepExecutor,
};
use korpis_core::workload::{Plan, PlanTarget};
use korpis_proto::korpis::v1 as pb;

fn status(s: PlanStatus) -> pb::PlanStatus {
    use pb::PlanStatus as P;
    match s {
        PlanStatus::Proposed => P::PLAN_STATUS_PROPOSED,
        PlanStatus::Approved => P::PLAN_STATUS_APPROVED,
        PlanStatus::Queued => P::PLAN_STATUS_QUEUED,
        PlanStatus::Applying => P::PLAN_STATUS_APPLYING,
        PlanStatus::Applied => P::PLAN_STATUS_APPLIED,
        PlanStatus::Failed => P::PLAN_STATUS_FAILED,
        PlanStatus::Cancelled => P::PLAN_STATUS_CANCELLED,
        PlanStatus::Rejected => P::PLAN_STATUS_REJECTED,
        PlanStatus::Expired => P::PLAN_STATUS_EXPIRED,
        PlanStatus::Superseded => P::PLAN_STATUS_SUPERSEDED,
    }
}

fn layer(l: Layer) -> pb::StepLayer {
    match l {
        Layer::Vocabulary => pb::StepLayer::STEP_LAYER_VOCABULARY,
        Layer::Driver => pb::StepLayer::STEP_LAYER_DRIVER,
        Layer::Recipe => pb::StepLayer::STEP_LAYER_RECIPE,
        Layer::Operator => pb::StepLayer::STEP_LAYER_OPERATOR,
    }
}

/// The wire form of a step.
#[must_use]
pub fn step_to_pb(s: &Step) -> pb::Step {
    pb::Step {
        index: s.index,
        action: s.action.as_str().to_owned(),
        executor: match s.executor {
            StepExecutor::ControlPlane => pb::StepExecutor::STEP_EXECUTOR_CONTROL_PLANE,
            StepExecutor::Node => pb::StepExecutor::STEP_EXECUTOR_NODE,
            StepExecutor::Extension => pb::StepExecutor::STEP_EXECUTOR_EXTENSION,
        }
        .into(),
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
        classified_by: layer(s.classified_by).into(),
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

/// The wire form of a plan.
#[must_use]
pub fn plan_to_pb(p: &Plan) -> pb::Plan {
    let (kind, to_intent, to_power) = match p.target {
        PlanTarget::Intent(i) => (pb::PlanKind::PLAN_KIND_INTENT, Some(i), None),
        PlanTarget::Power(v) => (pb::PlanKind::PLAN_KIND_POWER, None, Some(v)),
        PlanTarget::Retire => (pb::PlanKind::PLAN_KIND_RETIRE, None, None),
        PlanTarget::Migrate => (pb::PlanKind::PLAN_KIND_MIGRATE, None, None),
    };
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
        status: status(p.status).into(),
        completed: p.completed,
        apply_after: p.apply_after.map(timestamp).into(),
        expires_at: timestamp(p.expires_at).into(),
        approved_by: p.approved_by.as_ref().map(principal::to_pb).into(),
        approved_at: p.approved_at.map(timestamp).into(),
        proposed_under: wire_id(IdKind::Grant, p.proposed_under),
        approval: approval.into(),
        approval_scope: approval_scope.into(),
        ..Default::default()
    }
}
