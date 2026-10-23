// SPDX-License-Identifier: AGPL-3.0-or-later

//! A query probe's questions to the control plane, and their answers (§5 of
//! docs/16-extensions.md): asked up the session, naming the workload, the intent its object was
//! built from, the probe, and the rounds run so far, and answered down it with the health
//! provider's next round, its verdict, or why the probe is not judged this time. An answer that
//! does not come in time is no answer, and the attempt is unjudged.

use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::health_answer::Outcome;
use korpis_proto::korpis::v1 as pb;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Questions waiting for the control plane's answer, matched by their identifier.
#[derive(Debug)]
pub struct Queries {
    outbound: mpsc::Sender<agent::AgentMessage>,
    pending: Mutex<HashMap<String, oneshot::Sender<Outcome>>>,
}

impl Queries {
    /// Sends questions up `outbound`, the session's.
    #[must_use]
    pub fn new(outbound: mpsc::Sender<agent::AgentMessage>) -> Arc<Self> {
        Arc::new(Self { outbound, pending: Mutex::new(HashMap::new()) })
    }

    /// Asks what follows `rounds` in `q`'s exchange, waiting at most `within`; `None` when no
    /// answer came.
    pub(super) async fn ask(
        &self,
        q: &super::Query,
        rounds: &[pb::HealthRound],
        within: Duration,
    ) -> Option<Outcome> {
        let id = uuid::Uuid::now_v7().to_string();
        let (tx, rx) = oneshot::channel();
        self.lock().insert(id.clone(), tx);
        let probe = if q.liveness {
            agent::ProbeRole::PROBE_ROLE_LIVENESS
        } else {
            agent::ProbeRole::PROBE_ROLE_READINESS
        };
        let up = agent::HealthQuery {
            request: id.clone(),
            workload: q.workload.clone(),
            intent: q.intent.clone(),
            probe: probe.into(),
            rounds: rounds.to_vec(),
            within: korpis_wire::duration(within).into(),
            ..Default::default()
        };
        let message = agent::AgentMessage {
            message: Some(Up::HealthQuery(Box::new(up))),
            ..Default::default()
        };
        let answered = match self.outbound.send(message).await {
            Ok(()) => tokio::time::timeout(within, rx).await.ok().and_then(Result::ok),
            Err(_) => None,
        };
        self.lock().remove(&id);
        answered
    }

    /// Hands an answer to the question waiting for it; one nobody waits for any more is dropped.
    pub fn answer(&self, answer: agent::HealthAnswer) {
        let waiting = self.lock().remove(&answer.request);
        if let (Some(tx), Some(outcome)) = (waiting, answer.outcome) {
            let _ = tx.send(outcome);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<Outcome>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
