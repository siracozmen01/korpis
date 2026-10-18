// SPDX-License-Identifier: AGPL-3.0-or-later

//! Runs as the store keeps them (§4 of docs/05-scheduling.md): one per occurrence and per power
//! cycle, changed only while active, and pruned to what the policy keeps.

use crate::support::{create, fixture, now};
use korpis_app::page::Page;
use korpis_app::ports::{RunEnd, RunRead as _, RunWrite as _, Store as _, Tx as _};
use korpis_core::workload::run::{Cause, Run, RunState, RunStatus, Trigger};
use korpis_store::testing::{enroll, ready_node};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

fn manual(workload: Uuid, intent: Uuid, cycle: u64) -> Run {
    Run { cycle: Some(cycle), ..Run::pending(workload, intent, Trigger::Manual, now(), now()) }
}

fn ended(workload: Uuid, intent: Uuid, status: RunStatus, at: SystemTime) -> Run {
    Run {
        status,
        cause: Some(if status == RunStatus::Succeeded { Cause::Exit } else { Cause::NotStarted }),
        ended_at: Some(at),
        created_at: at,
        ..Run::pending(workload, intent, Trigger::Schedule, at, at)
    }
}

#[tokio::test]
async fn a_run_is_made_once_per_occurrence_and_cycle_and_ends_once() {
    let f = fixture().await;
    let (w, first) = create(&f, "backup").await;
    let node = ready_node(f.organization, "fra-3", &[]).unwrap();
    enroll(&f.store, &node).await.unwrap();
    let at = now();
    let nightly = Run::pending(w.id, first.id, Trigger::Schedule, at, at);
    let again = Run::pending(w.id, first.id, Trigger::Schedule, at, at);

    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.insert_run(&nightly).await.unwrap());
    assert!(!tx.insert_run(&again).await.unwrap(), "one run per occurrence");
    assert!(tx.insert_run(&manual(w.id, first.id, 0)).await.unwrap());
    assert!(!tx.insert_run(&manual(w.id, first.id, 0)).await.unwrap(), "one run per cycle");
    assert!(tx.start_run(nightly.id, node.id, at).await.unwrap());
    assert!(!tx.start_run(nightly.id, node.id, at).await.unwrap(), "started once");
    tx.commit().await.unwrap();
    assert_eq!(f.store.active_runs(w.id).await.unwrap().len(), 2);
    assert_eq!(f.store.workloads_with_runs().await.unwrap(), [w.id], "it has active runs");

    let end = RunEnd { status: RunStatus::Succeeded, cause: Cause::Exit, exit_status: Some(0), at };
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.end_run(nightly.id, end).await.unwrap());
    assert!(!tx.end_run(nightly.id, end).await.unwrap(), "ended once");
    let pending = RunEnd { status: RunStatus::Pending, ..end };
    assert!(tx.end_run(nightly.id, pending).await.is_err(), "an end is not active");
    tx.commit().await.unwrap();
    let read = f.store.run(nightly.id).await.unwrap().unwrap();
    assert_eq!(
        (read.status, read.cause, read.exit_status),
        (end.status, Some(Cause::Exit), Some(0))
    );
    assert_eq!((read.node, read.started_at, read.ended_at), (Some(node.id), Some(at), Some(at)));
    let listed = f.store.runs(w.id, Page { after: None, size: 10 }).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed[0].id > listed[1].id, "newest first");
}

#[tokio::test]
async fn the_run_state_is_locked_made_empty_and_kept() {
    let f = fixture().await;
    let (w, first) = create(&f, "backup").await;
    let mut tx = f.store.begin().await.unwrap();
    let empty = tx.lock_run_state(w.id).await.unwrap();
    assert_eq!(empty, RunState { workload: w.id, ..RunState::default() });
    let kept = RunState {
        workload: w.id,
        next_due: Some(now() + Duration::from_secs(60)),
        evaluated_from: Some(first.id),
        made_cycle: Some(3),
    };
    tx.set_run_state(&kept).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(f.store.run_state(w.id).await.unwrap(), Some(kept));
}

#[tokio::test]
async fn ended_runs_are_pruned_to_what_the_policy_keeps_and_active_ones_never() {
    let f = fixture().await;
    let (w, first) = create(&f, "backup").await;
    let start = now();
    let mut tx = f.store.begin().await.unwrap();
    let mut succeeded = Vec::new();
    for n in 0..5 {
        let at = start + Duration::from_secs(n * 60);
        let run = ended(w.id, first.id, RunStatus::Succeeded, at);
        tx.insert_run(&run).await.unwrap();
        succeeded.push(run.id);
    }
    for n in 5..9 {
        let at = start + Duration::from_secs(n * 60);
        tx.insert_run(&ended(w.id, first.id, RunStatus::Missed, at)).await.unwrap();
    }
    tx.insert_run(&manual(w.id, first.id, 1)).await.unwrap();
    let pruned = tx.prune_runs(w.id, 2, 3).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(pruned.len(), 4, "three succeeded and one missed");
    assert!(succeeded[..3].iter().all(|id| pruned.contains(id)), "the oldest succeeded");
    let left = f.store.runs(w.id, Page { after: None, size: 100 }).await.unwrap();
    let count = |s: RunStatus| left.iter().filter(|r| r.status == s).count();
    assert_eq!((count(RunStatus::Succeeded), count(RunStatus::Missed)), (2, 3));
    assert_eq!(count(RunStatus::Pending), 1, "an active run is never pruned");
}
