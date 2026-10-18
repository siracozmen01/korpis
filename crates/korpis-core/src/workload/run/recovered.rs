// SPDX-License-Identifier: AGPL-3.0-or-later

//! The runs a restored control plane recovers from the assignments its nodes present (§8 of
//! docs/03-state.md): a run the store lacks is recorded as its signed reference made it, running
//! on the node that presented it, and what the store keeps of the workload's schedule and power
//! cycles is moved past it, so the run maker never makes a second run for what is already
//! running.

use super::{Run, RunRef, RunState, RunStatus, Trigger};
use crate::workload::spec::{IntentBody, Schedule};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::time::SystemTime;
use uuid::Uuid;

/// The run `r` names, of `workload`, recorded at `now` as running on `node` under `intent`. When
/// it started is not in what was signed, so it is recorded as when it was recovered.
#[must_use]
pub fn recovered(r: &RunRef, workload: Uuid, intent: Uuid, node: Uuid, now: SystemTime) -> Run {
    Run {
        id: r.run,
        workload,
        intent,
        trigger: r.trigger,
        scheduled_for: r.scheduled_for,
        started_at: Some(now),
        ended_at: None,
        node: Some(node),
        status: RunStatus::Running,
        exit_status: None,
        attempt: r.attempt.max(1),
        cycle: r.cycle.filter(|_| r.trigger == Trigger::Manual),
        occurrences: 1,
        cause: None,
        created_at: now,
    }
}

/// Whether the store already went past `r`, a run it lacks, so that `r` is from an assignment
/// older than what the store holds and not a run made after the backup: a manual run for a cycle
/// no higher than the highest made, a scheduled run for an occurrence before the next one due,
/// or a retry or event run due before the newest run the store holds (`newest`, its
/// `scheduled_for`). A run made after the backup is past every one of these.
#[must_use]
pub fn settled(state: &RunState, r: &RunRef, newest: Option<SystemTime>) -> bool {
    match r.trigger {
        Trigger::Manual => state.made_cycle.is_some_and(|made| r.cycle.unwrap_or(0) <= made),
        Trigger::Schedule => state.next_due.is_some_and(|due| due > r.scheduled_for),
        Trigger::Retry | Trigger::Event => newest.is_some_and(|n| n > r.scheduled_for),
    }
}

/// `state` moved past each of `runs`: the highest power cycle made at least a manual run's, and
/// the next occurrence of the target's schedule, read in `zone`, after a scheduled run's, unless
/// it was evaluated from the target and is later already. A retry moves nothing: the run it
/// retries already did.
#[must_use]
pub fn past(
    mut state: RunState,
    runs: &[RunRef],
    (target, body): (Uuid, &IntentBody),
    zone: Option<&TimeZone>,
) -> RunState {
    let schedule = body.schedule.as_deref().and_then(|s| Schedule::parse(s).ok());
    for r in runs {
        match r.trigger {
            Trigger::Manual => {
                let cycle = r.cycle.unwrap_or(0);
                state.made_cycle = Some(state.made_cycle.map_or(cycle, |m| m.max(cycle)));
            }
            Trigger::Schedule => {
                let (Some(schedule), Some(zone)) = (&schedule, zone) else { continue };
                let after = Timestamp::try_from(r.scheduled_for).unwrap_or(Timestamp::MAX);
                let Some(next) = schedule.next_after(after, zone).map(SystemTime::from) else {
                    continue;
                };
                let kept = state.evaluated_from == Some(target)
                    && state.next_due.is_some_and(|due| due > next);
                if !kept {
                    state.next_due = Some(next);
                    state.evaluated_from = Some(target);
                }
            }
            Trigger::Retry | Trigger::Event => {}
        }
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::spec::{Lifecycle, fixture};
    use std::time::Duration;

    fn at(s: &str) -> SystemTime {
        s.parse::<Timestamp>().unwrap().into()
    }

    fn named(trigger: Trigger, scheduled_for: SystemTime, cycle: Option<u64>) -> RunRef {
        RunRef {
            run: Uuid::now_v7(),
            attempt: 1,
            trigger,
            scheduled_for,
            timeout: None,
            intent: Uuid::now_v7(),
            cycle,
        }
    }

    #[test]
    fn a_recovered_scheduled_run_moves_the_next_occurrence_past_it_and_never_back() {
        let target = Uuid::now_v7();
        let body = IntentBody {
            lifecycle: Lifecycle::Scheduled,
            schedule: Some("0 3 * * *".into()),
            ..fixture()
        };
        let stale = RunState {
            next_due: Some(at("2026-10-13T03:00:00Z")),
            evaluated_from: Some(target),
            ..RunState::default()
        };
        let running = named(Trigger::Schedule, at("2026-10-15T03:00:00Z"), None);
        let moved = past(
            stale.clone(),
            std::slice::from_ref(&running),
            (target, &body),
            Some(&TimeZone::UTC),
        );
        assert_eq!(moved.next_due, Some(at("2026-10-16T03:00:00Z")));
        let later = RunState { next_due: Some(at("2026-10-20T03:00:00Z")), ..stale };
        let kept = past(later.clone(), &[running], (target, &body), Some(&TimeZone::UTC));
        assert_eq!(kept, later, "a later evaluation stands");
    }

    #[test]
    fn a_run_the_store_went_past_is_settled_and_one_made_after_the_backup_is_not() {
        let state = RunState {
            next_due: Some(at("2026-10-16T03:00:00Z")),
            made_cycle: Some(2),
            ..RunState::default()
        };
        let old = named(Trigger::Schedule, at("2026-10-15T03:00:00Z"), None);
        let new = named(Trigger::Schedule, at("2026-10-16T03:00:00Z"), None);
        assert!(settled(&state, &old, None));
        assert!(!settled(&state, &new, None));
        let restarted = |cycle| named(Trigger::Manual, at("2026-10-15T03:00:00Z"), Some(cycle));
        assert!(settled(&state, &restarted(2), None));
        assert!(!settled(&state, &restarted(3), None));
        let retry = named(Trigger::Retry, at("2026-10-15T04:00:00Z"), None);
        assert!(settled(&state, &retry, Some(at("2026-10-16T03:00:00Z"))));
        assert!(!settled(&state, &retry, Some(at("2026-10-15T03:00:00Z"))));
        assert!(!settled(&RunState::default(), &old, None), "a store that never evaluated it");
    }

    #[test]
    fn a_recovered_manual_run_raises_the_cycle_made_and_is_recorded_running_where_presented() {
        let body = IntentBody { lifecycle: Lifecycle::Task, ..fixture() };
        let manual = named(Trigger::Manual, at("2026-10-15T03:00:00Z"), Some(4));
        let state = RunState { made_cycle: Some(2), ..RunState::default() };
        let moved = past(state, std::slice::from_ref(&manual), (Uuid::now_v7(), &body), None);
        assert_eq!(moved.made_cycle, Some(4));
        let (workload, node) = (Uuid::now_v7(), Uuid::now_v7());
        let now = at("2026-10-18T00:00:00Z") + Duration::from_secs(1);
        let run = recovered(&manual, workload, manual.intent, node, now);
        assert_eq!((run.status, run.node, run.cycle), (RunStatus::Running, Some(node), Some(4)));
        assert_eq!(run.started_at, Some(now));
    }
}
