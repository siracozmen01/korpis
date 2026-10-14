// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a backup or restore job is given (§5.5 of docs/06-storage.md): asked of the control plane
//! up the session when the step begins, naming the plan and the step, and handed to the helper,
//! which hands it to the job. The agent keeps none of it once the job has it.
//!
//! Asking never holds the reconciler's loop, which serves every workload on the node while the
//! control plane waits on a backup target: a step asks and moves on, and a later pass finds the
//! answer. A question unanswered for a minute is taken as lost, and asked again.

use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long an unanswered question stands before it is asked again.
const ANSWER_WITHIN: Duration = Duration::from_secs(60);

/// Why a job was given nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NoAccess {
    /// Refused for good: the step is not this node's, or the backup already ended.
    #[error("refused: {0}")]
    Refused(String),
    /// Not now: the control plane, or its backup target, did not answer; the step asks again.
    #[error("not now: {0}")]
    Later(String),
}

/// Where a step's question stands.
#[derive(Debug)]
enum Asked {
    /// Sent, as request `id`, at `at`.
    Waiting { id: String, at: Instant },
    /// Answered.
    Answered(Outcome),
}

/// The questions steps asked, by plan and step, and their answers.
#[derive(Debug)]
pub struct JobAccesses {
    outbound: mpsc::Sender<agent::AgentMessage>,
    asked: Mutex<HashMap<(String, u32), Asked>>,
}

impl JobAccesses {
    /// Sends questions up `outbound`, the session's.
    #[must_use]
    pub fn new(outbound: mpsc::Sender<agent::AgentMessage>) -> Arc<Self> {
        Arc::new(Self { outbound, asked: Mutex::new(HashMap::new()) })
    }

    /// The answer for step `step` of plan `plan`, once there is one; `None` while it is awaited,
    /// the question asked first if it was not, or was lost.
    ///
    /// # Errors
    /// The refusal, or that it may be had later, after which asking again asks anew.
    pub fn poll(&self, plan: &str, step: u32) -> Option<Result<agent::JobGrant, NoAccess>> {
        let key = (plan.to_owned(), step);
        let mut asked = self.lock();
        match asked.remove(&key) {
            Some(Asked::Answered(outcome)) => return Some(judged(outcome)),
            Some(Asked::Waiting { id, at }) if at.elapsed() < ANSWER_WITHIN => {
                asked.insert(key, Asked::Waiting { id, at });
                return None;
            }
            _ => {}
        }
        let id = uuid::Uuid::now_v7().to_string();
        let up = agent::JobAccessRequest {
            request: id.clone(),
            plan: plan.to_owned(),
            step,
            ..Default::default()
        };
        let message = agent::AgentMessage {
            message: Some(Up::JobAccessRequest(Box::new(up))),
            ..Default::default()
        };
        if self.outbound.try_send(message).is_err() {
            return Some(Err(NoAccess::Later("the session is not taking messages".into())));
        }
        asked.insert(key, Asked::Waiting { id, at: Instant::now() });
        None
    }

    /// Keeps an answer for the step that asked; one nobody waits for any more is dropped, and
    /// with it what it carried.
    pub fn answer(&self, answer: agent::JobAccess) {
        let Some(outcome) = answer.outcome else { return };
        let mut asked = self.lock();
        let key = asked.iter().find_map(|(k, a)| match a {
            Asked::Waiting { id, .. } if *id == answer.request => Some(k.clone()),
            _ => None,
        });
        if let Some(key) = key {
            asked.insert(key, Asked::Answered(outcome));
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, u32), Asked>> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn judged(outcome: Outcome) -> Result<agent::JobGrant, NoAccess> {
    match outcome {
        Outcome::Grant(g) => Ok(*g),
        Outcome::Refused(e) if e.retryable => Err(NoAccess::Later(e.reason)),
        Outcome::Refused(e) => Err(NoAccess::Refused(e.reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_proto::korpis::v1 as pb;

    fn refused(reason: &str, retryable: bool) -> Outcome {
        let e = pb::Error { reason: reason.into(), retryable, ..Default::default() };
        Outcome::Refused(Box::new(e))
    }

    /// The request the last question sent.
    fn sent(rx: &mut mpsc::Receiver<agent::AgentMessage>) -> agent::JobAccessRequest {
        match rx.try_recv().unwrap().message {
            Some(Up::JobAccessRequest(r)) => *r,
            other => panic!("{other:?}"),
        }
    }

    fn answered(jobs: &JobAccesses, request: &agent::JobAccessRequest, outcome: Outcome) {
        let request = request.request.clone();
        jobs.answer(agent::JobAccess { request, outcome: Some(outcome), ..Default::default() });
    }

    #[test]
    fn a_question_is_asked_once_and_its_answer_found_by_a_later_poll() {
        let (tx, mut rx) = mpsc::channel(8);
        let jobs = JobAccesses::new(tx);
        assert!(jobs.poll("pln_1", 0).is_none(), "asked, not answered");
        let first = sent(&mut rx);
        assert!(jobs.poll("pln_1", 0).is_none(), "still awaited");
        assert!(rx.try_recv().is_err(), "and not asked twice");
        let grant = agent::JobGrant { data_key: vec![7; 32], ..Default::default() };
        answered(&jobs, &first, Outcome::Grant(Box::new(grant)));
        assert_eq!(jobs.poll("pln_1", 0).unwrap().unwrap().data_key, vec![7; 32]);
        assert!(jobs.lock().is_empty(), "nothing kept once taken");
    }

    #[test]
    fn a_refusal_says_whether_to_ask_again_and_a_stray_answer_is_dropped() {
        let (tx, mut rx) = mpsc::channel(8);
        let jobs = JobAccesses::new(tx);
        jobs.poll("pln_1", 1);
        jobs.poll("pln_1", 2);
        let (later, never) = (sent(&mut rx), sent(&mut rx));
        answered(&jobs, &later, refused("provider.unavailable", true));
        answered(&jobs, &never, refused("job.not_assigned", false));
        let stray = agent::JobAccessRequest { request: "nobody".into(), ..Default::default() };
        answered(&jobs, &stray, refused("x", false));
        assert_eq!(
            jobs.poll("pln_1", 1),
            Some(Err(NoAccess::Later("provider.unavailable".into())))
        );
        assert_eq!(jobs.poll("pln_1", 2), Some(Err(NoAccess::Refused("job.not_assigned".into()))));
        assert!(jobs.lock().is_empty());
    }

    #[test]
    fn a_session_that_is_gone_is_a_reason_to_ask_later() {
        let (tx, rx) = mpsc::channel(8);
        drop(rx);
        let jobs = JobAccesses::new(tx);
        assert!(matches!(jobs.poll("pln_1", 0), Some(Err(NoAccess::Later(_)))));
    }
}
