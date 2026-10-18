// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{ON_TIME, Tended, Tending, tend};
use crate::workload::PowerState;
use crate::workload::run::{Cause, Run, RunState, RunStatus, Trigger};
use crate::workload::spec::{Concurrency, IntentBody, Lifecycle, RunPolicy, fixture};
use jiff::tz::TimeZone;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

fn at(s: &str) -> SystemTime {
    s.parse::<jiff::Timestamp>().unwrap().into()
}

fn nightly(policy: RunPolicy) -> IntentBody {
    IntentBody {
        lifecycle: Lifecycle::Scheduled,
        schedule: Some("0 3 * * *".into()),
        timezone: Some("UTC".into()),
        runs: Some(policy),
        ..fixture()
    }
}

struct Case {
    workload: Uuid,
    intent: Uuid,
    body: IntentBody,
    power: (PowerState, u64),
    state: RunState,
    active: Vec<Run>,
    newest: Option<Run>,
}

impl Case {
    fn new(body: IntentBody) -> Self {
        let (workload, intent) = (Uuid::now_v7(), Uuid::now_v7());
        let state = RunState { workload, evaluated_from: Some(intent), ..RunState::default() };
        Self {
            workload,
            intent,
            body,
            power: (PowerState::Running, 0),
            state,
            active: Vec::new(),
            newest: None,
        }
    }

    fn due(mut self, next: &str) -> Self {
        self.state.next_due = Some(at(next));
        self
    }

    fn running(mut self, scheduled_for: &str) -> Self {
        let at = at(scheduled_for);
        let run = Run::pending(self.workload, self.intent, Trigger::Schedule, at, at);
        self.active.push(Run { status: RunStatus::Running, started_at: Some(at), ..run });
        self
    }

    fn pending(mut self, scheduled_for: &str) -> Self {
        let at = at(scheduled_for);
        self.active.push(Run::pending(self.workload, self.intent, Trigger::Schedule, at, at));
        self
    }

    fn at(&self, now: &str) -> Tended {
        tend(Tending {
            now: at(now),
            workload: self.workload,
            target: Some((self.intent, &self.body)),
            power: Some(self.power),
            zone: Some(&TimeZone::UTC),
            state: self.state.clone(),
            active: self.active.clone(),
            newest: self.newest.clone(),
        })
    }
}

fn statuses(t: &Tended) -> Vec<(RunStatus, Option<Cause>, u32)> {
    t.made.iter().map(|r| (r.status, r.cause, r.occurrences)).collect()
}

#[test]
fn the_first_evaluation_finds_the_next_occurrence_and_makes_nothing() {
    let mut case = Case::new(nightly(RunPolicy::default()));
    case.state.evaluated_from = None;
    let t = case.at("2026-01-10T12:00:00Z");
    assert!(t.made.is_empty());
    assert_eq!(t.state.next_due, Some(at("2026-01-11T03:00:00Z")));
    assert_eq!(t.state.evaluated_from, Some(case.intent));
}

#[test]
fn an_occurrence_on_time_makes_a_run_and_a_changed_schedule_starts_from_now() {
    let case = Case::new(nightly(RunPolicy::default())).due("2026-01-11T03:00:00Z");
    let t = case.at("2026-01-11T03:00:30Z");
    assert_eq!(statuses(&t), [(RunStatus::Pending, None, 1)]);
    assert_eq!(t.made[0].scheduled_for, at("2026-01-11T03:00:00Z"));
    assert_eq!(t.state.next_due, Some(at("2026-01-12T03:00:00Z")));
    let mut changed = case;
    changed.state.evaluated_from = Some(Uuid::now_v7());
    let t = changed.at("2026-01-11T03:00:30Z");
    assert!(t.made.is_empty(), "no occurrence of an earlier schedule is made");
}

#[test]
fn nights_passed_while_down_are_one_skipped_run_or_caught_up_oldest_first() {
    let skip = Case::new(nightly(RunPolicy::default())).due("2026-01-11T03:00:00Z");
    let t = skip.at("2026-01-14T07:00:00Z");
    assert_eq!(statuses(&t), [(RunStatus::Missed, Some(Cause::Skipped), 4)]);
    assert_eq!(t.made[0].scheduled_for, at("2026-01-11T03:00:00Z"));
    assert_eq!(t.state.next_due, Some(at("2026-01-15T03:00:00Z")));

    let policy = RunPolicy { catch_up: true, ..RunPolicy::default() };
    let catch = Case::new(nightly(policy)).due("2026-01-11T03:00:00Z");
    let t = catch.at("2026-01-14T03:00:30Z");
    let due: Vec<_> = t.made.iter().map(|r| r.scheduled_for).collect();
    let nights = ["2026-01-11T03:00:00Z", "2026-01-12T03:00:00Z", "2026-01-13T03:00:00Z"];
    let mut expected: Vec<_> = nights.iter().map(|n| at(n)).collect();
    expected.push(at("2026-01-14T03:00:00Z"));
    assert_eq!(due, expected, "three caught up, then tonight's waits behind them");
    assert!(t.made.iter().all(|r| r.status == RunStatus::Pending));

    let replace =
        RunPolicy { catch_up: true, concurrency: Concurrency::Replace, ..RunPolicy::default() };
    let t = Case::new(nightly(replace)).due("2026-01-11T03:00:00Z").at("2026-01-13T07:00:00Z");
    assert_eq!(
        statuses(&t),
        [(RunStatus::Missed, Some(Cause::Replaced), 2), (RunStatus::Pending, None, 1)]
    );
}

#[test]
fn a_night_due_while_one_runs_follows_concurrency() {
    let forbid = Case::new(nightly(RunPolicy::default()))
        .due("2026-01-12T03:00:00Z")
        .running("2026-01-11T03:00:00Z");
    let t = forbid.at("2026-01-12T03:00:10Z");
    assert_eq!(statuses(&t), [(RunStatus::Missed, Some(Cause::Concurrency), 1)]);
    assert!(t.ended.is_empty(), "the active one goes on");

    let replace = RunPolicy { concurrency: Concurrency::Replace, ..RunPolicy::default() };
    let case =
        Case::new(nightly(replace)).due("2026-01-12T03:00:00Z").running("2026-01-11T03:00:00Z");
    let t = case.at("2026-01-12T03:00:10Z");
    assert_eq!(t.ended.len(), 1);
    assert_eq!((t.ended[0].1.status, t.ended[0].1.cause), (RunStatus::Cancelled, Cause::Replaced));
    assert_eq!(statuses(&t), [(RunStatus::Pending, None, 1)]);

    let allow = RunPolicy { concurrency: Concurrency::Allow, ..RunPolicy::default() };
    let case =
        Case::new(nightly(allow)).due("2026-01-12T03:00:00Z").running("2026-01-11T03:00:00Z");
    let t = case.at("2026-01-12T03:00:10Z");
    assert!(t.ended.is_empty());
    assert_eq!(statuses(&t), [(RunStatus::Pending, None, 1)]);
}

#[test]
fn a_run_never_started_gives_way_to_tonight_unless_catching_up() {
    let skip = Case::new(nightly(RunPolicy::default()))
        .due("2026-01-12T03:00:00Z")
        .pending("2026-01-11T03:00:00Z");
    let t = skip.at("2026-01-12T03:00:10Z");
    assert_eq!((t.ended[0].1.status, t.ended[0].1.cause), (RunStatus::Missed, Cause::NotStarted));
    assert_eq!(statuses(&t), [(RunStatus::Pending, None, 1)]);

    let policy = RunPolicy { catch_up: true, ..RunPolicy::default() };
    let wait =
        Case::new(nightly(policy)).due("2026-01-12T03:00:00Z").pending("2026-01-11T03:00:00Z");
    let t = wait.at("2026-01-12T03:00:10Z");
    assert!(t.ended.is_empty(), "it stays, and tonight's waits behind it");
    assert_eq!(statuses(&t), [(RunStatus::Pending, None, 1)]);
}

#[test]
fn stopping_power_cancels_every_active_run_and_suspends_the_schedule() {
    let mut case = Case::new(nightly(RunPolicy::default()))
        .due("2026-01-12T03:00:00Z")
        .running("2026-01-11T03:00:00Z")
        .pending("2026-01-11T04:00:00Z");
    case.power = (PowerState::Stopped, 0);
    let t = case.at("2026-01-12T05:00:00Z");
    assert!(t.made.is_empty(), "nothing passed while stopped is missed");
    assert!(
        t.ended.iter().all(|(_, e)| e.status == RunStatus::Cancelled && e.cause == Cause::Stopped)
    );
    assert_eq!(t.ended.len(), 2);
    assert_eq!((t.state.next_due, t.state.evaluated_from), (None, None));
}

#[test]
fn a_task_runs_once_per_cycle_and_a_scheduled_workload_from_its_first_restart() {
    let mut task = Case::new(IntentBody { lifecycle: Lifecycle::Task, ..fixture() });
    let t = task.at("2026-01-10T12:00:00Z");
    assert_eq!(t.made.len(), 1);
    assert_eq!((t.made[0].trigger, t.made[0].cycle), (Trigger::Manual, Some(0)));
    task.state = t.state;
    assert!(task.at("2026-01-10T12:01:00Z").made.is_empty(), "once per cycle");
    task.power.1 = 1;
    assert_eq!(task.at("2026-01-10T12:02:00Z").made[0].cycle, Some(1));

    let mut scheduled = Case::new(nightly(RunPolicy::default())).due("2026-01-11T03:00:00Z");
    assert!(scheduled.at("2026-01-10T12:00:00Z").made.is_empty(), "cycle zero is nothing");
    scheduled.power.1 = 2;
    let t = scheduled.at("2026-01-10T12:00:00Z");
    assert_eq!((t.made.len(), t.state.made_cycle), (1, Some(2)));
}

#[test]
fn a_failed_run_is_tried_again_after_its_backoff_while_attempts_remain() {
    let policy = RunPolicy { retries: 2, ..RunPolicy::default() };
    let mut case =
        Case::new(IntentBody { lifecycle: Lifecycle::Task, runs: Some(policy), ..fixture() });
    case.state.made_cycle = Some(0);
    let ended = at("2026-01-10T12:00:00Z");
    let failed = Run {
        status: RunStatus::Failed,
        cause: Some(Cause::Exit),
        ended_at: Some(ended),
        cycle: Some(0),
        ..Run::pending(case.workload, case.intent, Trigger::Manual, ended, ended)
    };
    case.newest = Some(failed.clone());
    assert!(case.at("2026-01-10T12:00:05Z").made.is_empty(), "within its backoff");
    let t = case.at("2026-01-10T12:00:10Z");
    assert_eq!((t.made[0].trigger, t.made[0].attempt), (Trigger::Retry, 2));
    assert_eq!(t.made[0].scheduled_for, ended + Duration::from_secs(10));
    case.newest = Some(Run { attempt: 3, ..failed });
    assert!(case.at("2026-01-11T12:00:00Z").made.is_empty(), "no attempts remain");
}

#[test]
fn an_occurrence_more_than_a_minute_late_is_missed_not_run() {
    let case = Case::new(nightly(RunPolicy::default())).due("2026-01-11T03:00:00Z");
    let late = at("2026-01-11T03:00:00Z") + ON_TIME + Duration::from_secs(1);
    let now = jiff::Timestamp::try_from(late).unwrap().to_string();
    let t = case.at(&now);
    assert_eq!(statuses(&t), [(RunStatus::Missed, Some(Cause::Skipped), 1)]);
}
