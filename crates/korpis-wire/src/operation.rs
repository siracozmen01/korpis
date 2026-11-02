// SPDX-License-Identifier: AGPL-3.0-or-later

//! Operations as the API shows them, and their kinds and policies as a client asks for them.

use crate::audit::map;
use crate::{Result, invalid, principal, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::operation::{
    Item, ItemState, Kind, OnFailure, Operation, Policy, Progress, Scope, Status,
};
use korpis_proto::korpis::v1 as pb;

enum_map!(kind_to_pb, kind_from_pb, Kind, pb::OperationKind, "an operation kind", {
    Kind::Drain => pb::OperationKind::OPERATION_KIND_DRAIN,
    Kind::Evacuate => pb::OperationKind::OPERATION_KIND_EVACUATE,
    Kind::TemplateRollout => pb::OperationKind::OPERATION_KIND_TEMPLATE_ROLLOUT,
});

enum_map!(status_to_pb, status_from_pb, Status, pb::OperationStatus, "an operation status", {
    Status::Proposed => pb::OperationStatus::OPERATION_STATUS_PROPOSED,
    Status::Approved => pb::OperationStatus::OPERATION_STATUS_APPROVED,
    Status::Running => pb::OperationStatus::OPERATION_STATUS_RUNNING,
    Status::Paused => pb::OperationStatus::OPERATION_STATUS_PAUSED,
    Status::Completed => pb::OperationStatus::OPERATION_STATUS_COMPLETED,
    Status::Failed => pb::OperationStatus::OPERATION_STATUS_FAILED,
    Status::Aborted => pb::OperationStatus::OPERATION_STATUS_ABORTED,
});

enum_map!(state_to_pb, state_from_pb, ItemState, pb::OperationItemState, "an item state", {
    ItemState::Waiting => pb::OperationItemState::OPERATION_ITEM_STATE_WAITING,
    ItemState::Applying => pb::OperationItemState::OPERATION_ITEM_STATE_APPLYING,
    ItemState::Settling => pb::OperationItemState::OPERATION_ITEM_STATE_SETTLING,
    ItemState::Done => pb::OperationItemState::OPERATION_ITEM_STATE_DONE,
    ItemState::Failed => pb::OperationItemState::OPERATION_ITEM_STATE_FAILED,
    ItemState::Skipped => pb::OperationItemState::OPERATION_ITEM_STATE_SKIPPED,
});

const fn on_failure_to_pb(o: OnFailure) -> pb::OnFailure {
    match o {
        OnFailure::Pause => pb::OnFailure::ON_FAILURE_PAUSE,
        OnFailure::Continue => pb::OnFailure::ON_FAILURE_CONTINUE,
        OnFailure::Abort => pb::OnFailure::ON_FAILURE_ABORT,
    }
}

/// The policy a client asks for, from the field `field`: an unset number is 1, an unset failure
/// policy is to pause.
pub fn policy_from_pb(field: &str, p: Option<&pb::OperationPolicy>) -> Result<Policy> {
    let Some(p) = p else { return Ok(Policy::default()) };
    let on_failure = match p.on_failure.as_known() {
        None | Some(pb::OnFailure::ON_FAILURE_UNSPECIFIED | pb::OnFailure::ON_FAILURE_PAUSE) => {
            OnFailure::Pause
        }
        Some(pb::OnFailure::ON_FAILURE_CONTINUE) => OnFailure::Continue,
        Some(pb::OnFailure::ON_FAILURE_ABORT) => OnFailure::Abort,
    };
    let policy = Policy {
        concurrency: p.concurrency.max(1),
        max_unavailable: p.max_unavailable.max(1),
        on_failure,
    };
    policy.validate().map_err(|e| invalid(&format!("{field}.{}", e.field()), e.to_string()))?;
    Ok(policy)
}

fn progress_to_pb(p: &Progress) -> pb::OperationProgress {
    pb::OperationProgress {
        total: p.total,
        waiting: p.waiting,
        applying: p.applying,
        settling: p.settling,
        done: p.done,
        failed: p.failed,
        skipped: p.skipped,
        ..Default::default()
    }
}

fn item_to_pb(kind: Kind, i: &Item) -> pb::OperationItem {
    let subject = match kind {
        Kind::Drain | Kind::Evacuate => IdKind::Workload,
        Kind::TemplateRollout => IdKind::Grant,
    };
    pb::OperationItem {
        subject: wire_id(subject, i.subject),
        state: state_to_pb(i.state).into(),
        plan: i.plan.map(|p| wire_id(IdKind::Plan, p)),
        issued: i.issued.map(|g| wire_id(IdKind::Grant, g)),
        detail: map(Some(&i.detail)),
        reason: i.reason.clone(),
        begun_at: i.begun_at.map(timestamp).into(),
        ended_at: i.ended_at.map(timestamp).into(),
        ..Default::default()
    }
}

/// The wire form of an operation with its progress over `items`, and the items themselves when
/// `shown`, which a list leaves out.
#[must_use]
pub fn operation_to_pb(op: &Operation, items: &[Item], shown: bool) -> pb::Operation {
    let (node, template, template_version) = match op.scope {
        Scope::Node(n) => (Some(wire_id(IdKind::Node, n)), None, 0),
        Scope::Template { template, version } => {
            (None, Some(wire_id(IdKind::Template, template)), version)
        }
    };
    let a = op.approved.as_ref();
    pb::Operation {
        id: wire_id(IdKind::Operation, op.id),
        kind: kind_to_pb(op.kind).into(),
        node,
        template,
        template_version,
        policy: pb::OperationPolicy {
            concurrency: op.policy.concurrency,
            max_unavailable: op.policy.max_unavailable,
            on_failure: on_failure_to_pb(op.policy.on_failure).into(),
            ..Default::default()
        }
        .into(),
        status: status_to_pb(op.status).into(),
        reason: op.reason.clone(),
        proposed_by: principal::to_pb(&op.proposed_by).into(),
        proposed_at: timestamp(op.proposed_at).into(),
        approved_by: a.map(|a| principal::to_pb(&a.by)).into(),
        approved_at: a.map(|a| timestamp(a.at)).into(),
        approved_under: a.map(|a| wire_id(IdKind::Grant, a.under)).unwrap_or_default(),
        start_after: a.and_then(|a| a.start_after).map(timestamp).into(),
        ended_at: op.ended_at.map(timestamp).into(),
        progress: progress_to_pb(&Progress::of(items)).into(),
        items: if shown { items.iter().map(|i| item_to_pb(op.kind, i)).collect() } else { vec![] },
        ..Default::default()
    }
}
