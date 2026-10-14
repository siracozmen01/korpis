// SPDX-License-Identifier: AGPL-3.0-or-later

//! The job queue (§6 of docs/03-state.md): queueing under one key once; one claimer at a time,
//! however many ask together; a lease that runs out handing the job to another, and the first
//! holder unable to end it after; a retry put back for its time, and failed once its attempts are
//! spent.

use crate::support::{fixture, now};
use korpis_app::ports::{JobEnd, JobRead, JobWrite, NewJob, Store, Tx};
use std::time::Duration;

const LEASE: Duration = Duration::from_secs(60);

async fn queued(store: &korpis_store::PgStore, key: &str, max: u32) -> bool {
    let mut tx = store.begin().await.unwrap();
    let job = NewJob {
        key,
        kind: "test",
        payload: serde_json::json!({ "key": key }),
        now: now(),
        due: now(),
        max_attempts: max,
    };
    let q = tx.enqueue_job(&job).await.unwrap();
    tx.commit().await.unwrap();
    q
}

async fn ended(store: &korpis_store::PgStore, job: (uuid::Uuid, &str), end: JobEnd) -> bool {
    let mut tx = store.begin().await.unwrap();
    let e = tx.end_job(job, &end, now()).await.unwrap();
    tx.commit().await.unwrap();
    e
}

#[tokio::test]
async fn a_job_is_queued_once_and_claimed_by_one_worker_at_a_time() {
    let f = fixture().await;
    assert!(queued(&f.store, "plan:1:0", 3).await);
    assert!(!queued(&f.store, "plan:1:0", 3).await, "the same key queues nothing");
    let claims = (0..8).map(|i| {
        let store = f.store.clone();
        tokio::spawn(async move {
            store.claim_job(&format!("w{i}"), &["test"], (now(), LEASE)).await.unwrap()
        })
    });
    let mut won = Vec::new();
    for c in claims {
        if let Some(job) = c.await.unwrap() {
            won.push(job);
        }
    }
    assert_eq!(won.len(), 1, "one claimer");
    assert_eq!((won[0].attempt, won[0].payload["key"].as_str()), (1, Some("plan:1:0")));
    assert!(f.store.claim_job("w9", &["other"], (now(), LEASE)).await.unwrap().is_none());
}

#[tokio::test]
async fn a_lease_that_runs_out_hands_the_job_on_and_the_first_holder_cannot_end_it() {
    let f = fixture().await;
    queued(&f.store, "plan:2:0", 3).await;
    let first = f.store.claim_job("a", &["test"], (now(), Duration::ZERO)).await.unwrap().unwrap();
    let second = f.store.claim_job("b", &["test"], (now(), LEASE)).await.unwrap().unwrap();
    assert_eq!((first.id, second.attempt), (second.id, 2), "the same job, its second attempt");
    assert!(!ended(&f.store, (first.id, "a"), JobEnd::Done).await, "a's lease is gone");
    assert!(ended(&f.store, (second.id, "b"), JobEnd::Done).await);
    assert!(f.store.claim_job("c", &["test"], (now(), LEASE)).await.unwrap().is_none(), "done");
}

#[tokio::test]
async fn a_retry_waits_for_its_time_and_fails_once_its_attempts_are_spent() {
    let f = fixture().await;
    queued(&f.store, "plan:3:0", 2).await;
    let job = f.store.claim_job("a", &["test"], (now(), LEASE)).await.unwrap().unwrap();
    let at = now() + Duration::from_secs(30);
    assert!(ended(&f.store, (job.id, "a"), JobEnd::Retry { at, error: "busy".into() }).await);
    assert!(f.store.claim_job("a", &["test"], (now(), LEASE)).await.unwrap().is_none(), "not yet");
    let again = f.store.claim_job("a", &["test"], (at, LEASE)).await.unwrap().unwrap();
    assert_eq!(again.attempt, 2);
    let later = at + Duration::from_secs(30);
    let retried = JobEnd::Retry { at: later, error: "still busy".into() };
    assert!(ended(&f.store, (again.id, "a"), retried).await);
    assert!(
        f.store.claim_job("a", &["test"], (later, LEASE)).await.unwrap().is_none(),
        "two attempts spent, failed"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM job WHERE id = $1")
        .bind(job.id)
        .fetch_one(f.store.pool())
        .await
        .unwrap();
    assert_eq!(status, "failed");
}

#[tokio::test]
async fn a_deferred_job_waits_without_spending_an_attempt() {
    let f = fixture().await;
    queued(&f.store, "sweep:1", 1).await;
    let at = now() + Duration::from_secs(60);
    for _ in 0..3 {
        let job = f.store.claim_job("a", &["test"], (at, LEASE)).await.unwrap().unwrap();
        assert_eq!(job.attempt, 1, "never counted");
        assert!(ended(&f.store, (job.id, "a"), JobEnd::Defer { at }).await);
    }
    let job = f.store.claim_job("a", &["test"], (at, LEASE)).await.unwrap().unwrap();
    assert!(ended(&f.store, (job.id, "a"), JobEnd::Done).await);
}
