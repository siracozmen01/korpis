// SPDX-License-Identifier: AGPL-3.0-or-later

//! Runs: the executions of a `task` or `scheduled` workload (§4 of docs/05-scheduling.md). The
//! control plane makes every run; a node starts and ends only the runs its assignment names.

mod recovered;
mod tend;

pub use recovered::{past, recovered, settled};
pub use tend::{ON_TIME, Tended, Tending, tend};

use std::time::SystemTime;
use uuid::Uuid;

/// What made a run (§4.1 of docs/05-scheduling.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// An occurrence of the schedule.
    Schedule,
    /// A higher power cycle.
    Manual,
    /// The next attempt of a failed run.
    Retry,
    /// An event; nothing makes one yet.
    Event,
}

/// Where a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Made, and not yet started by a node.
    Pending,
    /// Started by its node.
    Running,
    /// Ended with a successful exit status.
    Succeeded,
    /// Ended any other way its process could end.
    Failed,
    /// Ended by the control plane before its process did.
    Cancelled,
    /// Never made into an execution: an occurrence skipped, or a run its node never started.
    Missed,
}

impl RunStatus {
    /// Whether the run is active: pending or running (§4.3 of docs/05-scheduling.md).
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Pending | Self::Running)
    }
}

/// Why a run ended as it did (§4.2 to §4.4 of docs/05-scheduling.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// Its process exited, and the exit status was judged.
    Exit,
    /// Its node stopped it at its timeout.
    Timeout,
    /// Someone cancelled it.
    Cancelled,
    /// Its workload's power was stopped.
    Stopped,
    /// A newer run replaced it.
    Replaced,
    /// It came due while another was active, under `forbid`.
    Concurrency,
    /// Its node had not started it when its next occurrence came due.
    NotStarted,
    /// Its occurrences passed while the control plane was not making runs, under `catch_up:
    /// false`.
    Skipped,
    /// Its node lost the workload's lease.
    NodeLost,
}

macro_rules! names {
    ($ty:ty, { $($v:path => $s:literal),+ $(,)? }) => {
        impl $ty {
            /// Its stored name.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $($v => $s,)+ }
            }

            /// The value a stored name names.
            #[must_use]
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($s => Some($v),)+ _ => None }
            }
        }
    };
}

names!(Trigger, {
    Trigger::Schedule => "schedule",
    Trigger::Manual => "manual",
    Trigger::Retry => "retry",
    Trigger::Event => "event",
});

names!(RunStatus, {
    RunStatus::Pending => "pending",
    RunStatus::Running => "running",
    RunStatus::Succeeded => "succeeded",
    RunStatus::Failed => "failed",
    RunStatus::Cancelled => "cancelled",
    RunStatus::Missed => "missed",
});

names!(Cause, {
    Cause::Exit => "exit",
    Cause::Timeout => "timeout",
    Cause::Cancelled => "cancelled",
    Cause::Stopped => "stopped",
    Cause::Replaced => "replaced",
    Cause::Concurrency => "concurrency",
    Cause::NotStarted => "not_started",
    Cause::Skipped => "skipped",
    Cause::NodeLost => "node_lost",
});

/// One run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Its identifier.
    pub id: Uuid,
    /// The workload it is a run of.
    pub workload: Uuid,
    /// The intent it was made under, whose `exit` probe judges it.
    pub intent: Uuid,
    /// What made it.
    pub trigger: Trigger,
    /// The occurrence it is for, or when it was made.
    pub scheduled_for: SystemTime,
    /// When its node started it.
    pub started_at: Option<SystemTime>,
    /// When it ended.
    pub ended_at: Option<SystemTime>,
    /// The node it ran on: the one its workload was placed on when it started.
    pub node: Option<Uuid>,
    /// Where it stands.
    pub status: RunStatus,
    /// Its process's exit status, once it exited.
    pub exit_status: Option<i32>,
    /// Which attempt it is, from one.
    pub attempt: u32,
    /// The power cycle that requested it, for a manual run.
    pub cycle: Option<u64>,
    /// How many occurrences it stands for; above one only for a missed run recording an
    /// interval skipped.
    pub occurrences: u32,
    /// Why it ended as it did.
    pub cause: Option<Cause>,
    /// When the control plane made it.
    pub created_at: SystemTime,
}

impl Run {
    /// A pending run of `workload` under `intent`, made at `now` for `scheduled_for`.
    #[must_use]
    pub fn pending(
        workload: Uuid,
        intent: Uuid,
        trigger: Trigger,
        scheduled_for: SystemTime,
        now: SystemTime,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            workload,
            intent,
            trigger,
            scheduled_for,
            started_at: None,
            ended_at: None,
            node: None,
            status: RunStatus::Pending,
            exit_status: None,
            attempt: 1,
            cycle: None,
            occurrences: 1,
            cause: None,
            created_at: now,
        }
    }

    /// A run recorded missed at `now`, standing for `occurrences` occurrences from
    /// `scheduled_for`, for `cause`.
    #[must_use]
    pub fn missed(
        workload: Uuid,
        intent: Uuid,
        (scheduled_for, occurrences): (SystemTime, u32),
        cause: Cause,
        now: SystemTime,
    ) -> Self {
        Self {
            status: RunStatus::Missed,
            occurrences,
            cause: Some(cause),
            ended_at: Some(now),
            ..Self::pending(workload, intent, Trigger::Schedule, scheduled_for, now)
        }
    }
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunEnd {
    /// Its status: any but pending or running.
    pub status: RunStatus,
    /// Why.
    pub cause: Cause,
    /// Its process's exit status, when it exited.
    pub exit_status: Option<i32>,
    /// When.
    pub at: SystemTime,
}

/// How a run ended, as its node reported it at `at` (§4.2 of docs/05-scheduling.md): stopped at
/// its timeout, failed; stopped because it was no longer named, cancelled; and otherwise judged
/// by its exit status against the `exit` probe of the intent it was made under, zero alone when
/// that declares none. A run whose process never started, or whose exit status is unknown, has
/// failed.
#[must_use]
pub fn reported_end(
    body: &crate::workload::spec::IntentBody,
    cause: &str,
    exit_status: Option<i32>,
    at: SystemTime,
) -> RunEnd {
    use crate::workload::spec::health::ProbeKind;
    let end = |status, cause| RunEnd { status, cause, exit_status, at };
    match cause {
        "timeout" => return end(RunStatus::Failed, Cause::Timeout),
        "cancelled" => return end(RunStatus::Cancelled, Cause::Cancelled),
        _ => {}
    }
    let probes = [&body.health.readiness, &body.health.liveness];
    let codes = probes.into_iter().flatten().find_map(|p| match &p.kind {
        ProbeKind::Exit { success_codes } => Some(success_codes.clone()),
        _ => None,
    });
    let success = match (exit_status, codes) {
        (None, _) => false,
        (Some(code), Some(codes)) if !codes.is_empty() => codes.contains(&code),
        (Some(code), _) => code == 0,
    };
    let status = if success { RunStatus::Succeeded } else { RunStatus::Failed };
    end(status, Cause::Exit)
}

/// One run a node is to execute, as its assignment names it (§4.5 of docs/05-scheduling.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRef {
    /// The run.
    pub run: Uuid,
    /// Which attempt it is.
    pub attempt: u32,
    /// What made it.
    pub trigger: Trigger,
    /// The occurrence it is for.
    pub scheduled_for: SystemTime,
    /// How long it may take before the node stops it.
    pub timeout: Option<std::time::Duration>,
    /// The intent it was made under.
    pub intent: Uuid,
    /// The power cycle that requested it, for a manual run.
    pub cycle: Option<u64>,
}

/// The active runs a node is to execute, from `active` under `policy`: every one under `allow`;
/// otherwise the running ones, or else the oldest pending, so that runs queued behind one another
/// (caught up, or waiting behind one its node has not started) go one at a time.
#[must_use]
pub fn named(active: &[Run], policy: &crate::workload::spec::RunPolicy) -> Vec<RunRef> {
    use crate::workload::spec::Concurrency;
    let refer = |r: &Run| RunRef {
        run: r.id,
        attempt: r.attempt,
        trigger: r.trigger,
        scheduled_for: r.scheduled_for,
        timeout: policy.timeout,
        intent: r.intent,
        cycle: r.cycle,
    };
    let live = active.iter().filter(|r| r.status.is_active());
    if policy.concurrency == Concurrency::Allow {
        return live.map(refer).collect();
    }
    let running: Vec<RunRef> =
        live.clone().filter(|r| r.status == RunStatus::Running).map(refer).collect();
    if !running.is_empty() {
        return running;
    }
    live.min_by_key(|r| (r.scheduled_for, r.id)).map(refer).into_iter().collect()
}

/// What the control plane keeps of a workload's runs beside the runs themselves: the next
/// occurrence of its schedule and the intent it was evaluated from, and the highest power cycle
/// a run was made for, so that pruning a run never makes it again.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunState {
    /// The workload.
    pub workload: Uuid,
    /// The next occurrence of its schedule, once evaluated.
    pub next_due: Option<SystemTime>,
    /// The intent `next_due` was evaluated from.
    pub evaluated_from: Option<Uuid>,
    /// The highest power cycle a run was made for.
    pub made_cycle: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_reads_back_and_only_pending_and_running_are_active() {
        for s in [RunStatus::Pending, RunStatus::Running, RunStatus::Missed, RunStatus::Failed] {
            assert_eq!(RunStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(Cause::parse("not_started"), Some(Cause::NotStarted));
        assert_eq!(Trigger::parse("cron"), None);
        assert!(RunStatus::Pending.is_active() && RunStatus::Running.is_active());
        assert!(!RunStatus::Missed.is_active() && !RunStatus::Cancelled.is_active());
    }

    #[test]
    fn a_reported_end_is_judged_by_the_exit_probe_and_zero_alone_without_one() {
        use crate::workload::spec::health::{Probe, ProbeKind};
        use crate::workload::spec::{IntentBody, fixture};
        let at = SystemTime::UNIX_EPOCH;
        let plain = fixture();
        assert_eq!(reported_end(&plain, "exit", Some(0), at).status, RunStatus::Succeeded);
        assert_eq!(reported_end(&plain, "exit", Some(1), at).status, RunStatus::Failed);
        assert_eq!(reported_end(&plain, "exit", None, at).status, RunStatus::Failed);
        let timed = reported_end(&plain, "timeout", Some(143), at);
        assert_eq!((timed.status, timed.cause), (RunStatus::Failed, Cause::Timeout));
        let probe = Probe {
            kind: ProbeKind::Exit { success_codes: vec![0, 3] },
            period: None,
            timeout: None,
            failures: 0,
            initial_delay: None,
        };
        let mut lenient = IntentBody { ..plain };
        lenient.health.liveness = Some(probe);
        assert_eq!(reported_end(&lenient, "exit", Some(3), at).status, RunStatus::Succeeded);
    }

    #[test]
    fn a_node_runs_every_active_run_only_under_allow_and_otherwise_one_at_a_time() {
        use crate::workload::spec::{Concurrency, RunPolicy};
        let (w, i) = (Uuid::now_v7(), Uuid::now_v7());
        let at = |s: u64| SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(s);
        let later = Run::pending(w, i, Trigger::Schedule, at(20), at(20));
        let earlier = Run::pending(w, i, Trigger::Schedule, at(10), at(30));
        let queued = [later.clone(), earlier.clone()];
        let forbid = RunPolicy::default();
        let ids = |refs: Vec<RunRef>| refs.into_iter().map(|r| r.run).collect::<Vec<_>>();
        assert_eq!(ids(named(&queued, &forbid)), [earlier.id], "the oldest pending");
        let running = Run { status: RunStatus::Running, ..later.clone() };
        assert_eq!(ids(named(&[running.clone(), earlier.clone()], &forbid)), [running.id]);
        let allow = RunPolicy { concurrency: Concurrency::Allow, ..forbid };
        assert_eq!(named(&queued, &allow).len(), 2);
    }
}
