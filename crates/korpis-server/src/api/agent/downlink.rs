// SPDX-License-Identifier: AGPL-3.0-or-later

//! What goes down a session: the node's assignment set, recomputed whenever something it is made of
//! changes and sent when its digest differs from the last one sent. On the first pass a node
//! already holding the set is told `InSync` and nothing else. While a restore settles nothing is
//! sent. To an agent that takes deltas, a change sends only the assignments new or at a new
//! generation and the workloads gone, from the set last sent, which the agent holds by the time it
//! reads the delta, since the stream is ordered and it holds or answers each in turn; at the start
//! of a session, from the set it said it holds, when that is the one last sent to it from here.
//! Every other case, and an agent asking for a resync, sends the whole set, which is always
//! correct.

use crate::api::ServerApp;
use crate::sessions::{Sent, Sessions};
use korpis_app::node::AssignmentSet;
use korpis_core::id::IdKind;
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
    /// Whether the agent said it takes deltas.
    pub(super) takes_deltas: bool,
}

/// What the agent holds, or will once it reads what was sent.
enum Base {
    /// What it said it holds as the session began.
    Said(String),
    /// The set last sent.
    Sent(Sent),
    /// Nothing known: it asked for the whole set.
    Unknown,
}

fn signed(a: &korpis_app::ports::StoredAssignment) -> agent::SignedAssignment {
    agent::SignedAssignment {
        assignment: a.sealed.body.clone(),
        signature: a.sealed.signature.clone(),
        key_id: a.sealed.key_id.clone(),
        ..Default::default()
    }
}

/// What `set` is, as a base for the next delta.
fn sent(set: &AssignmentSet) -> Sent {
    let generations = set.assignments.iter().map(|a| (a.workload, a.generation)).collect();
    Sent { digest: set.digest.clone(), generations }
}

/// What changed from `base` to `set`.
fn delta_message(base: &Sent, set: &AssignmentSet) -> agent::ControlMessage {
    let upserts = set
        .assignments
        .iter()
        .filter(|a| base.generations.get(&a.workload) != Some(&a.generation))
        .map(signed)
        .collect();
    let now: std::collections::HashSet<Uuid> = set.assignments.iter().map(|a| a.workload).collect();
    let mut removed: Vec<String> = base
        .generations
        .keys()
        .filter(|w| !now.contains(w))
        .map(|w| korpis_wire::wire_id(IdKind::Workload, *w))
        .collect();
    removed.sort_unstable();
    let message = agent::AssignmentDelta {
        base_digest: base.digest.clone(),
        upserts,
        removed,
        set_digest: set.digest.clone(),
        ..Default::default()
    };
    agent::ControlMessage {
        message: Some(Down::AssignmentDelta(Box::new(message))),
        ..Default::default()
    }
}

fn set_message(set: &AssignmentSet) -> agent::ControlMessage {
    let assignments = set.assignments.iter().map(signed).collect();
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
        let mut base = Base::Said(self.held.clone());
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
            if let Some(wait) = self.settling().await {
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = self.tx.closed() => return,
                    _ = ended.wait_for(|e| *e) => return,
                }
                continue;
            }
            if !self.pass(&mut base).await {
                return;
            }
            tokio::select! {
                () = self.wake.notified() => {}
                () = self.tx.closed() => return,
                _ = ended.wait_for(|e| *e) => return,
            }
        }
    }

    /// How long to wait before sending anything, while a restore settles: the restored set is older
    /// than what the node runs, and it goes out once what nodes present is recovered and the window
    /// ends. A failure to tell is waited out too.
    async fn settling(&self) -> Option<std::time::Duration> {
        match self.app.settling().await {
            Ok(None) => None,
            Ok(Some(until)) => Some(until.duration_since(self.app.now()).unwrap_or_default()),
            Err(e) => {
                tracing::warn!(node = %self.node, error = %e, "whether a restore settles failed");
                Some(std::time::Duration::from_secs(1))
            }
        }
    }

    /// What to send the agent holding `base` for it to hold `set`; nothing when it already will.
    fn told(&self, base: &Base, set: &AssignmentSet) -> Option<agent::ControlMessage> {
        match base {
            Base::Sent(last) if last.digest == set.digest => None,
            Base::Said(held) if *held == set.digest => Some(in_sync(&set.digest)),
            Base::Sent(last) if self.takes_deltas => Some(delta_message(last, set)),
            Base::Said(held) if self.takes_deltas => {
                match self.sessions.remembered(self.node, held) {
                    Some(prior) => Some(delta_message(&prior, set)),
                    None => Some(set_message(set)),
                }
            }
            Base::Said(_) | Base::Sent(_) | Base::Unknown => Some(set_message(set)),
        }
    }

    /// One recomputation; false once the session's stream is gone.
    async fn pass(&self, base: &mut Base) -> bool {
        // Boxed: computing a node's set is a large future, held across every assignment.
        let computed = match Box::pin(self.app.sync_assignments(self.node)).await {
            Ok(s) => s,
            Err(e) => {
                // Retried on the next wake: every change and every resync wakes it.
                tracing::warn!(node = %self.node, error = %e, "assignment set failed");
                return true;
            }
        };
        if self.sessions.take_resync(self.node, self.id) {
            *base = Base::Unknown;
        }
        let Some(message) = self.told(base, &computed) else { return true };
        if self.tx.send(message).await.is_err() {
            return false;
        }
        let now = sent(&computed);
        self.sessions.remember(self.node, now.clone());
        *base = Base::Sent(now);
        true
    }
}
