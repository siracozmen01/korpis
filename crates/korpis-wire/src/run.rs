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
        intent: wire_id(IdKind::Intent, r.intent),
        cycle: r.cycle,
        ..Default::default()
    }
}

/// A run as an assignment names it, from its wire form.
///
/// # Errors
/// A field that does not translate, named.
pub fn ref_from_pb(r: &agent::RunRef) -> crate::Result<RunRef> {
    let trigger = match r.trigger.as_known() {
        Some(pb::RunTrigger::RUN_TRIGGER_SCHEDULE) => Trigger::Schedule,
        Some(pb::RunTrigger::RUN_TRIGGER_MANUAL) => Trigger::Manual,
        Some(pb::RunTrigger::RUN_TRIGGER_RETRY) => Trigger::Retry,
        Some(pb::RunTrigger::RUN_TRIGGER_EVENT) => Trigger::Event,
        _ => return Err(crate::invalid("trigger", "unknown")),
    };
    if r.cycle.is_some() != (trigger == Trigger::Manual) {
        return Err(crate::invalid("cycle", "exactly for a manual run"));
    }
    let scheduled_for =
        r.scheduled_for.as_option().ok_or_else(|| crate::invalid("scheduled_for", "required"))?;
    Ok(RunRef {
        run: crate::id(IdKind::Run, "run", &r.run)?,
        attempt: r.attempt,
        trigger,
        scheduled_for: crate::time("scheduled_for", scheduled_for)?,
        timeout: r.timeout.as_option().map(|d| crate::duration_from("timeout", d)).transpose()?,
        intent: crate::id(IdKind::Intent, "intent", &r.intent)?,
        cycle: r.cycle,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn a_run_reference_round_trips_and_a_manual_one_needs_its_cycle() {
        let r = RunRef {
            run: uuid::Uuid::now_v7(),
            attempt: 2,
            trigger: Trigger::Manual,
            scheduled_for: SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
            timeout: Some(Duration::from_secs(600)),
            intent: uuid::Uuid::now_v7(),
            cycle: Some(3),
        };
        assert_eq!(ref_from_pb(&ref_to_pb(&r)), Ok(r.clone()));
        let uncycled = agent::RunRef { cycle: None, ..ref_to_pb(&r) };
        assert_eq!(ref_from_pb(&uncycled).map_err(|e| e.field), Err("cycle".to_owned()));
    }
}
