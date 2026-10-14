// SPDX-License-Identifier: AGPL-3.0-or-later

//! The job queue (§6 of docs/03-state.md): work the control plane does itself, claimed by one
//! replica at a time. A job is queued in the transaction that decides it is due, ended in the
//! transaction that records what it did, and claimed alone, under a lease.

use super::StoreResult;
use korpis_core::workload::Plan;
use std::future::Future;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// A job, as a worker claimed it.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// The job.
    pub id: Uuid,
    /// What it is, which says what its payload holds.
    pub kind: String,
    /// What it works on.
    pub payload: serde_json::Value,
    /// This attempt, counted from one.
    pub attempt: u32,
    /// How many attempts it is given.
    pub max_attempts: u32,
}

/// A job to queue.
#[derive(Debug, Clone, PartialEq)]
pub struct NewJob<'a> {
    /// What makes it the same work as another: queueing a key twice queues it once.
    pub key: &'a str,
    /// What it is.
    pub kind: &'a str,
    /// What it works on.
    pub payload: serde_json::Value,
    /// When it is queued.
    pub now: SystemTime,
    /// When it is due.
    pub due: SystemTime,
    /// How many attempts it is given.
    pub max_attempts: u32,
}

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEnd {
    /// It is done.
    Done,
    /// It is to be tried again at `at`, why it was not done now said; failed instead once its
    /// attempts are spent.
    Retry {
        /// When.
        at: SystemTime,
        /// Why not now.
        error: String,
    },
    /// It is not to be tried again.
    Failed(String),
    /// It could not begin, for a reason that ends at `at`: put back for then, the attempt not
    /// counted, since nothing was tried.
    Defer {
        /// When.
        at: SystemTime,
    },
}

/// Queueing and ending jobs, in a transaction.
pub trait JobWrite {
    /// Queues `job`; whether it was queued, which a job of the same key already queued,
    /// running, or ended prevents.
    fn enqueue_job(&mut self, job: &NewJob<'_>) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Ends the attempt of job `id` that `worker` claimed, as `end` says; whether it still held
    /// the job, which a lease that ran out and another claim take away.
    fn end_job(
        &mut self,
        job_and_worker: (Uuid, &str),
        end: &JobEnd,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}

/// Claiming jobs, and finding the plan steps that become them.
pub trait JobRead {
    /// Plans that may have a control-plane step due: approved or applying, with one not yet
    /// completed (§4.9 of docs/02-architecture.md).
    fn control_plane_plans(&self) -> impl Future<Output = StoreResult<Vec<Plan>>> + Send;

    /// Claims, for `worker` and `lease`, one job of `kinds` that is due: queued and past its time,
    /// or running under a lease that ran out.
    fn claim_job(
        &self,
        worker: &str,
        kinds: &[&str],
        now_and_lease: (SystemTime, Duration),
    ) -> impl Future<Output = StoreResult<Option<Job>>> + Send;
}
