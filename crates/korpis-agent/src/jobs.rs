// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a backup or restore job is given (§5.5 of docs/06-storage.md): asked of the control plane
//! up the session when the step begins, naming the plan and the step, and handed to the helper,
//! which hands it to the job. The agent keeps none of it once the job has it.

use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::job_access::Outcome;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// How long a step waits for the control plane, which itself waits on the backup target.
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

/// Requests waiting for the control plane's answer, matched by their identifier.
#[derive(Debug)]
pub struct JobAccesses {
    outbound: mpsc::Sender<agent::AgentMessage>,
    pending: Mutex<HashMap<String, oneshot::Sender<Outcome>>>,
}

impl JobAccesses {
    /// Sends requests up `outbound`, the session's.
    #[must_use]
    pub fn new(outbound: mpsc::Sender<agent::AgentMessage>) -> Arc<Self> {
        Arc::new(Self { outbound, pending: Mutex::new(HashMap::new()) })
    }

    /// Asks for what step `step` of plan `plan` needs.
    ///
    /// # Errors
    /// The refusal, or that it may be had later.
    pub async fn request(&self, plan: &str, step: u32) -> Result<agent::JobGrant, NoAccess> {
        let id = uuid::Uuid::now_v7().to_string();
        let (tx, rx) = oneshot::channel();
        self.lock().insert(id.clone(), tx);
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
        let answered = match self.outbound.send(message).await {
            Ok(()) => tokio::time::timeout(ANSWER_WITHIN, rx).await.ok().and_then(Result::ok),
            Err(_) => None,
        };
        self.lock().remove(&id);
        match answered {
            Some(Outcome::Grant(g)) => Ok(*g),
            Some(Outcome::Refused(e)) if e.retryable => Err(NoAccess::Later(e.reason)),
            Some(Outcome::Refused(e)) => Err(NoAccess::Refused(e.reason)),
            None => Err(NoAccess::Later("the control plane did not answer".into())),
        }
    }

    /// Hands an answer to the request waiting for it; one nobody waits for any more is dropped,
    /// and with it what it carried.
    pub fn answer(&self, answer: agent::JobAccess) {
        let waiting = self.lock().remove(&answer.request);
        if let (Some(tx), Some(outcome)) = (waiting, answer.outcome) {
            let _ = tx.send(outcome);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<Outcome>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_proto::korpis::v1 as pb;

    /// Answers each request on `rx` as `answer` says.
    fn control_plane(
        rx: mpsc::Receiver<agent::AgentMessage>,
        jobs: Arc<JobAccesses>,
        answer: fn(u32) -> Outcome,
    ) {
        tokio::spawn(serve(rx, jobs, answer));
    }

    async fn serve(
        mut rx: mpsc::Receiver<agent::AgentMessage>,
        jobs: Arc<JobAccesses>,
        answer: fn(u32) -> Outcome,
    ) {
        while let Some(m) = rx.recv().await {
            if let Some(Up::JobAccessRequest(r)) = m.message {
                let outcome = Some(answer(r.step));
                jobs.answer(agent::JobAccess { request: r.request, outcome, ..Default::default() });
            }
        }
    }

    fn refused(reason: &str, retryable: bool) -> Outcome {
        Outcome::Refused(Box::new(pb::Error {
            reason: reason.into(),
            retryable,
            ..Default::default()
        }))
    }

    #[tokio::test]
    async fn each_request_gets_its_own_answer_and_a_refusal_says_whether_to_ask_again() {
        let (tx, rx) = mpsc::channel(8);
        let jobs = JobAccesses::new(tx);
        control_plane(rx, Arc::clone(&jobs), |step| match step {
            0 => Outcome::Grant(Box::new(agent::JobGrant {
                data_key: vec![7; 32],
                ..Default::default()
            })),
            1 => refused("provider.unavailable", true),
            _ => refused("job.not_assigned", false),
        });
        let (a, b, c) = tokio::join!(
            jobs.request("pln_1", 0),
            jobs.request("pln_1", 1),
            jobs.request("pln_1", 2)
        );
        assert_eq!(a.unwrap().data_key, vec![7; 32]);
        assert_eq!(b, Err(NoAccess::Later("provider.unavailable".into())));
        assert_eq!(c, Err(NoAccess::Refused("job.not_assigned".into())));
        assert!(jobs.lock().is_empty(), "nothing is kept once answered");
    }

    #[tokio::test]
    async fn a_session_that_is_gone_is_a_reason_to_ask_later() {
        let (tx, rx) = mpsc::channel(8);
        drop(rx);
        let jobs = JobAccesses::new(tx);
        assert!(matches!(jobs.request("pln_1", 0).await, Err(NoAccess::Later(_))));
    }
}
