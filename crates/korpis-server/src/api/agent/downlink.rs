// SPDX-License-Identifier: AGPL-3.0-or-later

//! What goes down a session: the node's assignment set (§4.1, §4.8 of
//! docs/02-architecture.md), recomputed whenever something it is made of changes and sent when
//! its digest differs from the last one sent. On the first pass a node already holding the set
//! is told `InSync` and nothing else (§9 of docs/10-api.md). Deltas are an optimization this
//! does not make: every change sends the whole set, which is always correct.

use crate::api::ServerApp;
use crate::sessions::Sessions;
use korpis_app::node::AssignmentSet;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

/// One session's downlink.
pub(super) struct Downlink {
    pub(super) app: Arc<ServerApp>,
    pub(super) sessions: Arc<Sessions>,
    pub(super) node: Uuid,
    pub(super) id: u64,
    pub(super) tx: Sender<agent::ControlMessage>,
    pub(super) wake: Arc<Notify>,
    /// Set once the session is replaced or forgotten.
    pub(super) ended: tokio::sync::watch::Receiver<bool>,
    /// The digest of the set the agent said it holds.
    pub(super) held: String,
}

fn set_message(set: &AssignmentSet) -> agent::ControlMessage {
    let assignments = set
        .assignments
        .iter()
        .map(|a| agent::SignedAssignment {
            assignment: a.sealed.body.clone(),
            signature: a.sealed.signature.clone(),
            key_id: a.sealed.key_id.clone(),
            ..Default::default()
        })
        .collect();
    let message =
        agent::AssignmentSet { assignments, set_digest: set.digest.clone(), ..Default::default() };
    agent::ControlMessage {
        message: Some(Down::AssignmentSet(Box::new(message))),
        ..Default::default()
    }
}

fn in_sync(digest: &str) -> agent::ControlMessage {
    let message = agent::InSync { set_digest: digest.to_owned(), ..Default::default() };
    agent::ControlMessage { message: Some(Down::InSync(Box::new(message))), ..Default::default() }
}

impl Downlink {
    /// Sends the set until the session ends, another replaces it, or its node is removed, which
    /// ends the session: the node's messages are not handled once it is no longer current.
    pub(super) async fn run(self) {
        let mut last: Option<String> = None;
        let mut ended = self.ended.clone();
        loop {
            if !self.sessions.is_current(self.node, self.id) {
                return;
            }
            if matches!(self.app.session_admitted(self.node).await, Ok(false)) {
                tracing::info!(node = %self.node, "the node was removed; its session ends");
                self.sessions.unregister(self.node, self.id);
                return;
            }
            if !self.pass(&mut last).await {
                return;
            }
            tokio::select! {
                () = self.wake.notified() => {}
                () = self.tx.closed() => return,
                _ = ended.wait_for(|e| *e) => return,
            }
        }
    }

    /// One recomputation; false once the session's stream is gone.
    async fn pass(&self, last: &mut Option<String>) -> bool {
        let computed = match self.app.sync_assignments(self.node).await {
            Ok(s) => s,
            Err(e) => {
                // Retried on the next wake: every change and every resync wakes it.
                tracing::warn!(node = %self.node, error = %e, "assignment set failed");
                return true;
            }
        };
        if last.as_deref() == Some(computed.digest.as_str()) {
            return true;
        }
        let message = if last.is_none() && computed.digest == self.held {
            in_sync(&computed.digest)
        } else {
            set_message(&computed)
        };
        if self.tx.send(message).await.is_err() {
            return false;
        }
        *last = Some(computed.digest);
        true
    }
}
