// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the control plane does with one workload's runs at one moment (§4 of
//! docs/05-scheduling.md): which runs to make, which to end, and what to keep of its schedule. It
//! is a function of what the store holds, the time, and the zone, so every rule of §4.1 to §4.4
//! is evaluated here and nowhere else, and each can be tested at any moment of any year.

use super::{Cause, Run, RunEnd, RunState, RunStatus, Trigger};
use crate::workload::PowerState;
use crate::workload::spec::runs::MAX_CATCH_UP;
use crate::workload::spec::{Concurrency, IntentBody, Lifecycle, RunPolicy, Schedule};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::collections::VecDeque;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// How late an occurrence's run may be made and still be on time (§4.3 of
/// docs/05-scheduling.md).
pub const ON_TIME: Duration = Duration::from_secs(60);
/// The most occurrences counted into one missed run; past it, the rest are passed over uncounted.
const MOST_COUNTED: u32 = 100_000;

/// What the store holds of one workload, at `now`.
#[derive(Debug)]
pub struct Tending<'a> {
    /// The moment.
    pub now: SystemTime,
    /// The workload.
    pub workload: Uuid,
    /// Its target intent, while it is live and has one.
    pub target: Option<(Uuid, &'a IntentBody)>,
    /// Its power target: desired state and cycle.
    pub power: Option<(PowerState, u64)>,
    /// The zone its target's schedule is read in.
    pub zone: Option<&'a TimeZone>,
    /// What is kept of its runs.
    pub state: RunState,
    /// Its active runs, oldest first.
    pub active: Vec<Run>,
    /// Its newest run, of any status.
    pub newest: Option<Run>,
    /// Its lease as the control plane holds it: the node holding it, and when it ends; none
    /// when no node holds it.
    pub lease: Option<(Uuid, SystemTime)>,
}

/// What to do.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tended {
    /// Runs to add: pending ones, and ones recorded missed as they are made.
    pub made: Vec<Run>,
    /// Active runs to end, and how.
    pub ended: Vec<(Uuid, RunEnd)>,
    /// What to keep of its runs.
    pub state: RunState,
}

impl Tended {
    /// Whether anything is to change.
    #[must_use]
    pub fn changes(&self, from: &RunState) -> bool {
        !self.made.is_empty() || !self.ended.is_empty() || self.state != *from
    }
}

struct Work {
    now: SystemTime,
    workload: Uuid,
    policy: RunPolicy,
    active: Vec<Run>,
    out: Tended,
}

/// What to do with a workload's runs, as `t` describes them.
#[must_use]
pub fn tend(t: Tending<'_>) -> Tended {
    let mut w = Work {
        now: t.now,
        workload: t.workload,
        policy: RunPolicy::default(),
        active: t.active,
        out: Tended { state: t.state, ..Tended::default() },
    };
    w.lost(t.lease);
    let target = t.target.filter(|(_, b)| b.lifecycle != Lifecycle::Persistent);
    let (Some((intent, body)), Some((PowerState::Running, cycle))) = (target, t.power) else {
        w.stop_all();
        return w.out;
    };
    w.policy = body.run_policy();
    w.manual(intent, body.lifecycle, cycle);
    let schedule = body.schedule.as_deref().and_then(|s| Schedule::parse(s).ok());
    if let (Lifecycle::Scheduled, Some(schedule), Some(zone)) = (body.lifecycle, schedule, t.zone) {
        w.schedule(intent, &schedule, zone);
    }
    w.retry(intent, t.newest.as_ref());
    w.out
}

fn instant(t: SystemTime) -> Timestamp {
    Timestamp::try_from(t).unwrap_or(Timestamp::MAX)
}

impl Work {
    /// Every running run whose node no longer holds the workload's lease has failed, its node
    /// lost (§4.2 of docs/05-scheduling.md); a retry is made from it like any failure.
    fn lost(&mut self, lease: Option<(Uuid, SystemTime)>) {
        let held =
            |r: &Run| lease.is_some_and(|(node, until)| r.node == Some(node) && until > self.now);
        let lost: Vec<Uuid> = self
            .active
            .iter()
            .filter(|r| r.status == RunStatus::Running && !held(r))
            .map(|r| r.id)
            .collect();
        for id in lost {
            self.end(id, RunStatus::Failed, Cause::NodeLost);
        }
    }

    /// No run is wanted: power is stopped, or the workload has none to run. Every active run is
    /// cancelled, and the schedule resumes from the first occurrence once runs are wanted again.
    fn stop_all(&mut self) {
        for id in self.active.iter().map(|r| r.id).collect::<Vec<_>>() {
            self.end(id, RunStatus::Cancelled, Cause::Stopped);
        }
        self.out.state.next_due = None;
        self.out.state.evaluated_from = None;
    }

    /// A run for a higher power cycle: a task's for every cycle from its first, a scheduled
    /// workload's for every cycle after zero.
    fn manual(&mut self, intent: Uuid, lifecycle: Lifecycle, cycle: u64) {
        let wanted = match (lifecycle, self.out.state.made_cycle) {
            (Lifecycle::Persistent, _) => false,
            (Lifecycle::Task, None) => true,
            (Lifecycle::Scheduled, None) => cycle > 0,
            (_, Some(made)) => cycle > made,
        };
        if wanted {
            self.out.state.made_cycle = Some(cycle);
            let run = Run::pending(self.workload, intent, Trigger::Manual, self.now, self.now);
            self.admit(Run { cycle: Some(cycle), ..run }, false);
        }
    }

    /// The occurrences that came due since the last evaluation: the late ones as `catch_up`
    /// says, and the newest, when it is on time, as concurrency says.
    fn schedule(&mut self, intent: Uuid, schedule: &Schedule, zone: &TimeZone) {
        let next = |after: SystemTime| schedule.next_after(instant(after), zone).map(Into::into);
        let state = &mut self.out.state;
        let due = state.next_due.filter(|_| state.evaluated_from == Some(intent));
        state.evaluated_from = Some(intent);
        let Some(first) = due else {
            state.next_due = next(self.now);
            return;
        };
        if first > self.now {
            return;
        }
        let mut newest = VecDeque::from([first]);
        let mut count = 1u32;
        while let Some(at) = newest.back().copied().and_then(next).filter(|at| *at <= self.now) {
            if count == MOST_COUNTED {
                break;
            }
            count += 1;
            newest.push_back(at);
            if newest.len() > usize::try_from(MAX_CATCH_UP).unwrap_or(usize::MAX) + 1 {
                newest.pop_front();
            }
        }
        self.out.state.next_due = next(self.now);
        let last = newest.back().copied().unwrap_or(first);
        let on_time = self.now.duration_since(last).is_ok_and(|late| late <= ON_TIME);
        if on_time {
            newest.pop_back();
            count -= 1;
        }
        self.late(intent, (first, count), newest.into());
        if on_time {
            let run = Run::pending(self.workload, intent, Trigger::Schedule, last, self.now);
            self.admit(run, true);
        }
    }

    /// `count` occurrences from `first` that passed while no run was made for them, the newest
    /// of them in `newest`, oldest first.
    fn late(&mut self, intent: Uuid, (first, count): (SystemTime, u32), newest: Vec<SystemTime>) {
        if count == 0 {
            return;
        }
        let missed = |at, n, cause| Run::missed(self.workload, intent, (at, n), cause, self.now);
        if !self.policy.catch_up {
            self.out.made.push(missed(first, count, Cause::Skipped));
            return;
        }
        let caught = u32::try_from(newest.len()).unwrap_or(u32::MAX);
        if self.policy.concurrency == Concurrency::Replace {
            if count > 1 {
                self.out.made.push(missed(first, count - 1, Cause::Replaced));
            }
            if let Some(at) = newest.last() {
                let run = Run::pending(self.workload, intent, Trigger::Schedule, *at, self.now);
                self.admit(run, false);
            }
            return;
        }
        if count > caught {
            self.out.made.push(missed(first, count - caught, Cause::Skipped));
        }
        // Catching up is not a concurrency conflict: they wait, oldest first, as it allows.
        for at in newest {
            self.make(Run::pending(self.workload, intent, Trigger::Schedule, at, self.now));
        }
    }

    /// The next attempt of the newest run, when it failed with attempts left and its backoff
    /// has passed, and nothing newer was made.
    fn retry(&mut self, intent: Uuid, newest: Option<&Run>) {
        let Some(last) = newest.filter(|r| r.status == RunStatus::Failed) else { return };
        if last.attempt > self.policy.retries || !self.out.made.is_empty() {
            return;
        }
        let Some(due) = last.ended_at.map(|at| at + self.policy.retry_after(last.attempt)) else {
            return;
        };
        if due <= self.now {
            let run = Run::pending(self.workload, intent, Trigger::Retry, due, self.now);
            self.admit(Run { attempt: last.attempt + 1, ..run }, false);
        }
    }

    /// Makes `run` as concurrency allows. An occurrence coming due under `catch_up: false`
    /// first ends every run its node has not started.
    fn admit(&mut self, run: Run, occurrence: bool) {
        if occurrence && !self.policy.catch_up {
            let pending = self.active.iter().filter(|r| r.status == RunStatus::Pending);
            for id in pending.map(|r| r.id).collect::<Vec<_>>() {
                self.end(id, RunStatus::Missed, Cause::NotStarted);
            }
        }
        let running = self.active.iter().any(|r| r.status == RunStatus::Running);
        match self.policy.concurrency {
            Concurrency::Forbid if running => {
                let missed = Run {
                    status: RunStatus::Missed,
                    cause: Some(Cause::Concurrency),
                    ended_at: Some(self.now),
                    ..run
                };
                self.out.made.push(missed);
            }
            Concurrency::Replace => {
                for id in self.active.iter().map(|r| r.id).collect::<Vec<_>>() {
                    self.end(id, RunStatus::Cancelled, Cause::Replaced);
                }
                self.make(run);
            }
            // Allowed beside the active ones, or forbidden with none running: made, and a
            // pending one ahead of it is started first (§4.5 of docs/05-scheduling.md).
            Concurrency::Allow | Concurrency::Forbid => self.make(run),
        }
    }

    fn make(&mut self, run: Run) {
        self.active.push(run.clone());
        self.out.made.push(run);
    }

    fn end(&mut self, id: Uuid, status: RunStatus, cause: Cause) {
        self.active.retain(|r| r.id != id);
        let end = RunEnd { status, cause, exit_status: None, at: self.now };
        self.out.ended.push((id, end));
    }
}

#[cfg(test)]
mod tests;
