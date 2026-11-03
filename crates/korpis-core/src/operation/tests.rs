// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use std::time::Duration;

fn proposed(on_failure: OnFailure) -> Operation {
    Operation {
        id: Uuid::nil(),
        kind: Kind::Drain,
        scope: Scope::Node(Uuid::nil()),
        policy: Policy { concurrency: 2, max_unavailable: 3, on_failure },
        status: Status::Proposed,
        reason: String::new(),
        proposed_by: Principal::User(Uuid::nil()),
        proposed_under: Uuid::nil(),
        proposed_at: SystemTime::UNIX_EPOCH,
        approved: None,
        ended_at: None,
        version: 0,
    }
}

fn running(on_failure: OnFailure) -> Operation {
    let mut op = proposed(on_failure);
    op.approve(approval(None)).unwrap();
    assert!(op.start(SystemTime::UNIX_EPOCH));
    op
}

fn approval(start_after: Option<SystemTime>) -> Approved {
    Approved {
        by: Principal::User(Uuid::max()),
        under: Uuid::max(),
        mfa: true,
        at: SystemTime::UNIX_EPOCH,
        start_after,
    }
}

fn progress(states: &[ItemState]) -> Progress {
    let items: Vec<Item> = states
        .iter()
        .zip(0..)
        .map(|(s, n)| Item { state: *s, ..Item::waiting(Uuid::nil(), Uuid::nil(), n) })
        .collect();
    Progress::of(&items)
}

#[test]
fn an_approved_operation_starts_no_earlier_than_it_was_approved_to() {
    let later = SystemTime::UNIX_EPOCH + Duration::from_secs(3600);
    let mut op = proposed(OnFailure::Pause);
    op.approve(approval(Some(later))).unwrap();
    assert!(!op.start(later - Duration::from_secs(1)));
    assert_eq!(op.status, Status::Approved);
    assert!(op.start(later));
    assert_eq!(op.status, Status::Running);
}

#[test]
fn a_failed_item_pauses_continues_or_fails_the_operation_as_its_policy_says() {
    let now = SystemTime::UNIX_EPOCH;
    let mut paused = running(OnFailure::Pause);
    paused.item_failed("no node fits", now);
    assert_eq!((paused.status, paused.reason.as_str()), (Status::Paused, "no node fits"));
    let mut continued = running(OnFailure::Continue);
    continued.item_failed("no node fits", now);
    assert_eq!(continued.status, Status::Running);
    let mut aborted = running(OnFailure::Abort);
    aborted.item_failed("no node fits", now);
    assert_eq!((aborted.status, aborted.ended_at), (Status::Failed, Some(now)));
}

#[test]
fn an_operation_ends_failed_when_any_item_failed_and_completed_otherwise() {
    let now = SystemTime::UNIX_EPOCH;
    let mut op = running(OnFailure::Continue);
    assert!(!op.conclude(&progress(&[ItemState::Done, ItemState::Settling]), now));
    let mut clean = op.clone();
    assert!(clean.conclude(&progress(&[ItemState::Done, ItemState::Skipped]), now));
    assert_eq!(clean.status, Status::Completed);
    assert!(op.conclude(&progress(&[ItemState::Done, ItemState::Failed]), now));
    assert_eq!((op.status, op.reason.as_str()), (Status::Failed, "1 of 2 items failed"));
}

#[test]
fn an_ended_operation_is_never_resumed_or_aborted_again() {
    let mut op = running(OnFailure::Pause);
    op.abort("enough", SystemTime::UNIX_EPOCH).unwrap();
    assert!(op.resume().is_err());
    assert!(op.abort("again", SystemTime::UNIX_EPOCH).is_err());
    assert!(op.pause("again").is_err());
}

#[test]
fn items_begin_within_both_concurrency_and_max_unavailable() {
    use ItemState::{Applying, Settling, Waiting};
    let policy = Policy { concurrency: 2, max_unavailable: 3, on_failure: OnFailure::Pause };
    let admit = |states: &[ItemState]| Admission::under(&policy, &progress(states)).0;
    assert_eq!(admit(&[Waiting; 5]), 2, "concurrency bounds it");
    assert_eq!(admit(&[Applying, Waiting, Waiting]), 1);
    assert_eq!(admit(&[Settling, Settling, Waiting, Waiting]), 1, "settling ones are unavailable");
    assert_eq!(admit(&[Settling, Settling, Applying, Waiting]), 0);
    assert_eq!(admit(&[Waiting]), 1, "no more than are waiting");
}

#[test]
fn a_policy_that_would_begin_nothing_or_everything_at_once_is_refused() {
    let p = |concurrency, max_unavailable| Policy {
        concurrency,
        max_unavailable,
        on_failure: OnFailure::Pause,
    };
    assert_eq!(p(0, 1).validate(), Err(PolicyError::Concurrency));
    assert_eq!(p(MOST_CONCURRENT + 1, 1).validate(), Err(PolicyError::Concurrency));
    assert_eq!(p(1, 0).validate(), Err(PolicyError::MaxUnavailable));
    assert_eq!(p(MOST_CONCURRENT, 1).validate(), Ok(()));
}
