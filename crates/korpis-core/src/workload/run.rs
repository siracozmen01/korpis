// SPDX-License-Identifier: AGPL-3.0-or-later

//! Runs: the executions of a `task` or `scheduled` workload (§4 of docs/05-scheduling.md). The
//! control plane makes every run; a node starts and ends only the runs its assignment names.

mod tend;

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
}
