// SPDX-License-Identifier: AGPL-3.0-or-later

//! Runs: `korpis.v1.Run` (§4 of docs/05-scheduling.md).

use crate::{duration, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::run::{Cause, Run, RunRef, RunStatus, Trigger};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;

/// The wire form of a trigger.
#[must_use]
pub const fn trigger_to_pb(t: Trigger) -> pb::RunTrigger {
    match t {
        Trigger::Schedule => pb::RunTrigger::RUN_TRIGGER_SCHEDULE,
        Trigger::Manual => pb::RunTrigger::RUN_TRIGGER_MANUAL,
        Trigger::Retry => pb::RunTrigger::RUN_TRIGGER_RETRY,
        Trigger::Event => pb::RunTrigger::RUN_TRIGGER_EVENT,
    }
}

/// The wire form of a run's status.
#[must_use]
pub const fn status_to_pb(s: RunStatus) -> pb::RunStatus {
    match s {
        RunStatus::Pending => pb::RunStatus::RUN_STATUS_PENDING,
        RunStatus::Running => pb::RunStatus::RUN_STATUS_RUNNING,
        RunStatus::Succeeded => pb::RunStatus::RUN_STATUS_SUCCEEDED,
        RunStatus::Failed => pb::RunStatus::RUN_STATUS_FAILED,
        RunStatus::Cancelled => pb::RunStatus::RUN_STATUS_CANCELLED,
        RunStatus::Missed => pb::RunStatus::RUN_STATUS_MISSED,
    }
}

/// The wire form of why a run ended.
#[must_use]
pub const fn cause_to_pb(c: Cause) -> pb::RunCause {
    match c {
        Cause::Exit => pb::RunCause::RUN_CAUSE_EXIT,
        Cause::Timeout => pb::RunCause::RUN_CAUSE_TIMEOUT,
        Cause::Cancelled => pb::RunCause::RUN_CAUSE_CANCELLED,
        Cause::Stopped => pb::RunCause::RUN_CAUSE_STOPPED,
        Cause::Replaced => pb::RunCause::RUN_CAUSE_REPLACED,
        Cause::Concurrency => pb::RunCause::RUN_CAUSE_CONCURRENCY,
        Cause::NotStarted => pb::RunCause::RUN_CAUSE_NOT_STARTED,
        Cause::Skipped => pb::RunCause::RUN_CAUSE_SKIPPED,
        Cause::NodeLost => pb::RunCause::RUN_CAUSE_NODE_LOST,
    }
}

/// The wire form of a run.
#[must_use]
pub fn run_to_pb(r: &Run) -> pb::Run {
    pb::Run {
        id: wire_id(IdKind::Run, r.id),
        workload: wire_id(IdKind::Workload, r.workload),
        intent: wire_id(IdKind::Intent, r.intent),
        trigger: trigger_to_pb(r.trigger).into(),
        scheduled_for: timestamp(r.scheduled_for).into(),
        started_at: r.started_at.map(timestamp).into(),
        ended_at: r.ended_at.map(timestamp).into(),
        node: r.node.map(|n| wire_id(IdKind::Node, n)).unwrap_or_default(),
        status: status_to_pb(r.status).into(),
        exit_status: r.exit_status,
        attempt: r.attempt,
        cycle: r.cycle,
        occurrences: r.occurrences,
        cause: r.cause.map_or(pb::RunCause::RUN_CAUSE_UNSPECIFIED, cause_to_pb).into(),
        created_at: timestamp(r.created_at).into(),
        ..Default::default()
    }
}

/// The wire form of a run as an assignment names it.
#[must_use]
pub fn ref_to_pb(r: &RunRef) -> agent::RunRef {
    agent::RunRef {
        run: wire_id(IdKind::Run, r.run),
        attempt: r.attempt,
        trigger: trigger_to_pb(r.trigger).into(),
        scheduled_for: timestamp(r.scheduled_for).into(),
        timeout: r.timeout.map(duration).into(),
        ..Default::default()
    }
}
