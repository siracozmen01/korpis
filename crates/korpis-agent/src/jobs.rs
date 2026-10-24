// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a backup or restore job is given (§5.5 of docs/06-storage.md): asked of the control plane
//! up the session when the step begins, naming the plan and the step, or the session reading a
//! block device's backup, and handed to the helper, which hands it to the job. The agent keeps
//! none of it once the job has it.
//!
//! Each question names a one-time recipient whose secret stays in this process: under tenant
//! custody the data key comes sealed to it, and is opened here, as the answer arrives, for the
//! context `<repository>/<workload>` (§5.5), so no one else, the control plane included, holds it.
//!
//! Asking never holds the reconciler's loop, which serves every workload on the node while the
//! control plane waits on a backup target: a step asks and moves on, and a later pass finds the
//! answer. A question unanswered for a minute is taken as lost, and asked again.

use korpis_extension::keyseal::Recipient;
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
    /// Sent, as request `id`, at `at`, naming `recipient`.
    Waiting { id: String, at: Instant, recipient: Recipient },
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
        let up = agent::JobAccessRequest { plan: plan.to_owned(), step, ..Default::default() };
        self.ask((plan.to_owned(), step), up)
    }

    /// As [`poll`](Self::poll), for offloading `workload`'s console stream (§4 of
    /// docs/14-streams.md).
    ///
    /// # Errors
    /// As [`poll`](Self::poll).
    pub fn poll_offload(&self, workload: &str) -> Option<Result<agent::JobGrant, NoAccess>> {
        let up = agent::JobAccessRequest { offload: workload.to_owned(), ..Default::default() };
        self.ask((format!("offload/{workload}"), u32::MAX), up)
    }

    /// As [`poll`](Self::poll), for session `session` reading a block device's backup (§5.4 of
    /// docs/06-storage.md).
    ///
    /// # Errors
    /// As [`poll`](Self::poll).
    pub fn poll_backup_read(
        &self,
        session: &str,
        read: agent::BackupRead,
    ) -> Option<Result<agent::JobGrant, NoAccess>> {
        let up = agent::JobAccessRequest { backup_read: read.into(), ..Default::default() };
        self.ask((format!("read/{session}"), u32::MAX), up)
    }

    fn ask(
        &self,
        key: (String, u32),
        mut up: agent::JobAccessRequest,
    ) -> Option<Result<agent::JobGrant, NoAccess>> {
        let mut asked = self.lock();
        match asked.remove(&key) {
            Some(Asked::Answered(outcome)) => return Some(judged(outcome)),
            Some(Asked::Waiting { id, at, recipient }) if at.elapsed() < ANSWER_WITHIN => {
                asked.insert(key, Asked::Waiting { id, at, recipient });
                return None;
            }
            _ => {}
        }
        let Ok(recipient) = Recipient::new() else {
            return Some(Err(NoAccess::Later("no randomness for a recipient".into())));
        };
        let id = uuid::Uuid::now_v7().to_string();
        up.request.clone_from(&id);
        up.recipient = recipient.public().to_vec();
        let message = agent::AgentMessage {
            message: Some(Up::JobAccessRequest(Box::new(up))),
            ..Default::default()
        };
        if self.outbound.try_send(message).is_err() {
            return Some(Err(NoAccess::Later("the session is not taking messages".into())));
        }
        asked.insert(key, Asked::Waiting { id, at: Instant::now(), recipient });
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
        let Some(key) = key else { return };
        if let Some(Asked::Waiting { recipient, .. }) = asked.remove(&key) {
            asked.insert(key, Asked::Answered(opened(outcome, &recipient)));
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, u32), Asked>> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `outcome` with a sealed key opened by `recipient`; one that does not open is refused, to be
/// asked for again.
fn opened(outcome: Outcome, recipient: &Recipient) -> Outcome {
    let Outcome::Grant(mut g) = outcome else { return outcome };
    if g.sealed_data_key.is_empty() {
        return Outcome::Grant(g);
    }
    let context = format!("{}/{}", g.repository, g.workload);
    match recipient.open(&g.sealed_data_key, context.as_bytes()) {
        Ok(key) => {
            g.data_key = key.to_vec();
            g.sealed_data_key.clear();
            Outcome::Grant(g)
        }
        Err(e) => Outcome::Refused(Box::new(korpis_proto::korpis::v1::Error {
            reason: "keys.unopened".into(),
            message: e.to_string(),
            retryable: true,
            ..Default::default()
        })),
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
    fn a_sealed_key_is_opened_as_it_arrives_and_only_for_its_context() {
        let (tx, mut rx) = mpsc::channel(8);
        let jobs = JobAccesses::new(tx);
        jobs.poll("pln_1", 0);
        jobs.poll("pln_1", 1);
        let (one, two) = (sent(&mut rx), sent(&mut rx));
        assert_eq!(one.recipient.len(), 32, "every question names a recipient");
        assert_ne!(one.recipient, two.recipient, "a new one each time");
        let sealed =
            |r: &[u8], context: &[u8]| korpis_extension::keyseal::seal(r, &[4; 32], context);
        let grant = |sealed_data_key| agent::JobGrant {
            sealed_data_key,
            repository: "rep_1".into(),
            workload: "wl_1".into(),
            ..Default::default()
        };
        let good = grant(sealed(&one.recipient, b"rep_1/wl_1").unwrap());
        answered(&jobs, &one, Outcome::Grant(Box::new(good)));
        let opened = jobs.poll("pln_1", 0).unwrap().unwrap();
        assert_eq!((opened.data_key, opened.sealed_data_key), (vec![4; 32], Vec::new()));
        let elsewhere = grant(sealed(&two.recipient, b"rep_1/wl_2").unwrap());
        answered(&jobs, &two, Outcome::Grant(Box::new(elsewhere)));
        assert_eq!(jobs.poll("pln_1", 1), Some(Err(NoAccess::Later("keys.unopened".into()))));
    }

    #[test]
    fn a_session_that_is_gone_is_a_reason_to_ask_later() {
        let (tx, rx) = mpsc::channel(8);
        drop(rx);
        let jobs = JobAccesses::new(tx);
        assert!(matches!(jobs.poll("pln_1", 0), Some(Err(NoAccess::Later(_)))));
    }
}
