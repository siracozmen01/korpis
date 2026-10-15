// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workload identity on the node (§9.5 of docs/08-identity.md, §6 of
//! docs/02-architecture.md): each workload gets a socket of its own, and a request on it goes up
//! the control stream naming the workload the socket was made for. The control plane decides;
//! the agent passes the answer back and keeps nothing.

mod socket;

pub use socket::{ControlPlane, IdentitySockets, MOUNT_POINT};

use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::workload_credential::Outcome;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// How long a workload waits for the control plane to answer before it is told to retry.
const ANSWER_WITHIN: Duration = Duration::from_secs(10);

/// Why a workload got no credential.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NoCredential {
    /// The control plane refused, with its reason.
    #[error("refused: {0}")]
    Refused(String),
    /// The control plane did not answer in time, or the stream is down; the node keeps running
    /// its workloads meanwhile (§4.6 of docs/02-architecture.md), and the workload asks again.
    #[error("the control plane did not answer; ask again")]
    Unavailable,
}

/// Requests waiting for the control plane's answer, matched by their identifier.
#[derive(Debug)]
pub struct Credentials {
    outbound: mpsc::Sender<agent::AgentMessage>,
    pending: Mutex<HashMap<String, oneshot::Sender<Outcome>>>,
}

impl Credentials {
    /// Sends requests up `outbound`, the session's.
    #[must_use]
    pub fn new(outbound: mpsc::Sender<agent::AgentMessage>) -> Arc<Self> {
        Arc::new(Self { outbound, pending: Mutex::new(HashMap::new()) })
    }

    /// Asks the control plane for `workload`'s credential.
    ///
    /// # Errors
    /// The refusal, or that no answer came.
    pub async fn request(&self, workload: &str) -> Result<agent::Credential, NoCredential> {
        let id = uuid::Uuid::now_v7().to_string();
        let (tx, rx) = oneshot::channel();
        self.lock().insert(id.clone(), tx);
        let up = agent::CredentialRequest {
            request: id.clone(),
            workload: workload.to_owned(),
            ..Default::default()
        };
        let message = agent::AgentMessage {
            message: Some(Up::CredentialRequest(Box::new(up))),
            ..Default::default()
        };
        let answered = match self.outbound.send(message).await {
            Ok(()) => tokio::time::timeout(ANSWER_WITHIN, rx).await.ok().and_then(Result::ok),
            Err(_) => None,
        };
        self.lock().remove(&id);
        match answered {
            Some(Outcome::Credential(c)) => Ok(*c),
            // Not now, for a reason that passes: the workload asks again, as for no answer.
            Some(Outcome::Refused(e)) if e.retryable => Err(NoCredential::Unavailable),
            Some(Outcome::Refused(e)) => Err(NoCredential::Refused(e.reason)),
            None => Err(NoCredential::Unavailable),
        }
    }

    /// Hands an answer to the request waiting for it; one nobody waits for any more is dropped.
    pub fn answer(&self, answer: agent::WorkloadCredential) {
        let waiting = self.lock().remove(&answer.request);
        if let (Some(tx), Some(outcome)) = (waiting, answer.outcome) {
            let _ = tx.send(outcome);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, oneshot::Sender<Outcome>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
