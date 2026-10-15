// SPDX-License-Identifier: AGPL-3.0-or-later

//! What an install's provider steps resolved to (§4 of docs/09-recipes.md): asked of the control
//! plane up the session, naming the plan, the install step, and the provider action, and answered
//! down it with the artifacts to fetch and write. The control plane calls the step provider, which
//! may take a while.
//!
//! Asking never holds the reconciler's loop: an install asks and moves on, and a later pass finds
//! the answer. A question unanswered for longer than the control plane waits on a provider is
//! taken as lost, and asked again.

use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::step_resolution::Outcome;
use korpis_proto::korpis::v1 as pb;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long an unanswered question stands before it is asked again: longer than the control plane
/// waits on a step provider, so a slow provider is not asked twice.
const ANSWER_WITHIN: Duration = Duration::from_secs(150);

/// Why a step resolved to nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unresolved {
    /// Refused for good, with the reason: the install fails, saying so.
    #[error("refused: {0}")]
    Refused(String),
    /// Not now: the control plane or its provider did not answer, or asked to be asked again.
    #[error("not now: {0}")]
    Later(String),
}

/// One provider action of one plan's install step.
pub type Asking = (String, u32, u32);

/// Where an action's question stands.
#[derive(Debug)]
enum Asked {
    /// Sent, as request `id`, at `at`.
    Waiting { id: String, at: Instant },
    /// Answered.
    Answered(Outcome),
}

/// The questions installs asked, by plan, step, and action, and their answers.
#[derive(Debug)]
pub struct StepAnswers {
    outbound: mpsc::Sender<agent::AgentMessage>,
    asked: Mutex<HashMap<Asking, Asked>>,
}

impl StepAnswers {
    /// Sends questions up `outbound`, the session's.
    #[must_use]
    pub fn new(outbound: mpsc::Sender<agent::AgentMessage>) -> Arc<Self> {
        Arc::new(Self { outbound, asked: Mutex::new(HashMap::new()) })
    }

    /// What `(plan, step, action)` resolved to on `platform`, once answered; `None` while it is
    /// awaited, the question asked first if it was not, or was lost. An answer stays until the
    /// install that asked [settles](Self::settled) it, so an install with several provider steps
    /// loses none while it waits for the rest; a "later" is handed out once, and asked anew.
    ///
    /// # Errors
    /// The refusal, or that it may be had later.
    pub fn poll(
        &self,
        (plan, step, action): (&str, u32, u32),
        platform: &pb::Platform,
    ) -> Option<Result<pb::StepResult, Unresolved>> {
        let key = (plan.to_owned(), step, action);
        let mut asked = self.lock();
        match asked.remove(&key) {
            Some(Asked::Answered(outcome)) => {
                let judged = judged(outcome.clone());
                if !matches!(judged, Err(Unresolved::Later(_))) {
                    asked.insert(key, Asked::Answered(outcome));
                }
                return Some(judged);
            }
            Some(Asked::Waiting { id, at }) if at.elapsed() < ANSWER_WITHIN => {
                asked.insert(key, Asked::Waiting { id, at });
                return None;
            }
            _ => {}
        }
        let id = uuid::Uuid::now_v7().to_string();
        let up = agent::StepRequest {
            request: id.clone(),
            plan: plan.to_owned(),
            step,
            action,
            platform: platform.clone().into(),
            ..Default::default()
        };
        let message = agent::AgentMessage {
            message: Some(Up::StepRequest(Box::new(up))),
            ..Default::default()
        };
        if self.outbound.try_send(message).is_err() {
            return Some(Err(Unresolved::Later("the session is not taking messages".into())));
        }
        asked.insert(key, Asked::Waiting { id, at: Instant::now() });
        None
    }

    /// Keeps an answer for the install that asked; one nobody waits for any more is dropped.
    pub fn answer(&self, answer: agent::StepResolution) {
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

    /// Forgets every answer for `step` of `plan`: the install keeps them now.
    pub fn settled(&self, plan: &str, step: u32) {
        self.lock().retain(|(p, s, _), _| !(p == plan && *s == step));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Asking, Asked>> {
        self.asked.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn judged(outcome: Outcome) -> Result<pb::StepResult, Unresolved> {
    match outcome {
        Outcome::Result(r) => Ok(*r),
        Outcome::Refused(e) if e.retryable => Err(Unresolved::Later(e.reason)),
        Outcome::Refused(e) => Err(Unresolved::Refused(e.reason)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, reason = "a failing test should fail loudly")]

    use super::*;

    fn platform() -> pb::Platform {
        pb::Platform { os: "linux".into(), arch: "amd64".into(), ..Default::default() }
    }

    fn sent(rx: &mut mpsc::Receiver<agent::AgentMessage>) -> agent::StepRequest {
        match rx.try_recv().unwrap().message {
            Some(Up::StepRequest(r)) => *r,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_question_is_asked_once_answered_once_and_a_stray_answer_is_dropped() {
        let (tx, mut rx) = mpsc::channel(8);
        let steps = StepAnswers::new(tx);
        assert!(steps.poll(("pln", 2, 3), &platform()).is_none());
        let r = sent(&mut rx);
        assert_eq!((r.plan.as_str(), r.step, r.action), ("pln", 2, 3));
        assert!(steps.poll(("pln", 2, 3), &platform()).is_none(), "still awaited");
        assert!(rx.try_recv().is_err(), "and not asked twice");

        steps.answer(agent::StepResolution { request: "nobody".into(), ..Default::default() });
        let result = pb::StepResult { resolved: "manifest 1".into(), ..Default::default() };
        steps.answer(agent::StepResolution {
            request: r.request.clone(),
            outcome: Some(Outcome::Result(Box::new(result))),
            ..Default::default()
        });
        let got = steps.poll(("pln", 2, 3), &platform()).unwrap().unwrap();
        assert_eq!(got.resolved, "manifest 1");
        let kept = steps.poll(("pln", 2, 3), &platform()).unwrap().unwrap();
        assert_eq!(kept, got, "kept until the install settles it");
        steps.settled("pln", 2);
        assert!(steps.poll(("pln", 2, 3), &platform()).is_none(), "then asked anew");

        let again = sent(&mut rx);
        let refused = pb::Error { reason: "step.provider_changed".into(), ..Default::default() };
        steps.answer(agent::StepResolution {
            request: again.request,
            outcome: Some(Outcome::Refused(Box::new(refused))),
            ..Default::default()
        });
        let e = steps.poll(("pln", 2, 3), &platform()).unwrap().unwrap_err();
        assert_eq!(e, Unresolved::Refused("step.provider_changed".into()));
    }
}
