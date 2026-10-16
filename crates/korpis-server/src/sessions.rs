// SPDX-License-Identifier: AGPL-3.0-or-later

//! The agent sessions this process holds: one per node, so what the control plane decides for a
//! node can be pushed down the stream that node opened (§4.2 of docs/02-architecture.md). A new
//! session for a node replaces the old one, whose outbound stream then ends: an agent that
//! reconnected after a network change must not leave a half-open twin receiving its
//! assignments. A session replaced or forgotten is told so at once, and every task serving it
//! stops, so its stream ends then rather than at the next message one of them happens to handle.

use korpis_app::ports::Feasible;
use korpis_proto::korpis::agent::v1::{ControlMessage, FeasibilityResult};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::mpsc::Sender;
use tokio::sync::{Notify, oneshot, watch};
use uuid::Uuid;

struct Entry {
    id: u64,
    tx: Sender<ControlMessage>,
    wake: Arc<Notify>,
    /// Set once the session is replaced or forgotten.
    ended: watch::Sender<bool>,
    /// Whether the agent said it answers feasibility queries.
    answers: bool,
}

/// The registry.
#[derive(Default)]
pub struct Sessions {
    open: Mutex<HashMap<Uuid, Entry>>,
    next: AtomicU64,
    /// Reachability probes sent and not yet answered, by probe: where the node's answer goes,
    /// which is empty when it is listening and says why not when it is not.
    probes: Mutex<HashMap<String, oneshot::Sender<String>>>,
    /// Feasibility queries sent and not yet answered, by query: the node asked, which alone may
    /// answer, and where its driver's answer goes (§2.1 of docs/05-scheduling.md).
    queries: Mutex<HashMap<String, (Uuid, oneshot::Sender<Feasible>)>>,
}

impl std::fmt::Debug for Sessions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sessions").field("open", &self.guard().len()).finish()
    }
}

impl Sessions {
    // A panic while the lock was held leaves a map that is still consistent: every operation
    // replaces or removes one entry whole.
    fn guard(&self) -> MutexGuard<'_, HashMap<Uuid, Entry>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers the session of `node` sending through `tx`, replacing any other, and whether
    /// its agent `answers` feasibility queries; its id, what wakes it to recompute the node's
    /// set, and what says it ended.
    pub fn register(
        &self,
        node: Uuid,
        tx: Sender<ControlMessage>,
        answers: bool,
    ) -> (u64, Arc<Notify>, watch::Receiver<bool>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let wake = Arc::new(Notify::new());
        let (ended, ending) = watch::channel(false);
        let entry = Entry { id, tx, wake: Arc::clone(&wake), ended, answers };
        if let Some(old) = self.guard().insert(node, entry) {
            old.ended.send_replace(true);
        }
        (id, wake, ending)
    }

    /// Wakes `node`'s session, if it has one here, to recompute its set. A wake while it is
    /// computing is kept, so a change made meanwhile is not missed.
    pub fn wake(&self, node: Uuid) {
        if let Some(e) = self.guard().get(&node) {
            e.wake.notify_one();
        }
    }

    /// Wakes every session.
    pub fn wake_all(&self) {
        for e in self.guard().values() {
            e.wake.notify_one();
        }
    }

    /// Whether session `id` is still the one `node` holds.
    #[must_use]
    pub fn is_current(&self, node: Uuid, id: u64) -> bool {
        self.guard().get(&node).is_some_and(|e| e.id == id)
    }

    /// Forgets session `id` of `node`, if it is still the current one.
    pub fn unregister(&self, node: Uuid, id: u64) {
        let mut open = self.guard();
        if open.get(&node).is_some_and(|e| e.id == id)
            && let Some(gone) = open.remove(&node)
        {
            gone.ended.send_replace(true);
        }
    }

    /// Where to send to `node`, if it has a session here.
    #[must_use]
    pub fn sender(&self, node: Uuid) -> Option<Sender<ControlMessage>> {
        self.guard().get(&node).map(|e| e.tx.clone())
    }

    /// Where the answer to `probe` will arrive, once the node has been sent it.
    pub fn expect_probe(&self, probe: &str) -> oneshot::Receiver<String> {
        let (tx, rx) = oneshot::channel();
        self.probes.lock().unwrap_or_else(PoisonError::into_inner).insert(probe.to_owned(), tx);
        rx
    }

    /// A node's answer to `probe`: empty when it is listening, otherwise why it is not.
    pub fn probe_listening(&self, probe: &str, error: String) {
        let waiting = self.probes.lock().unwrap_or_else(PoisonError::into_inner).remove(probe);
        if let Some(tx) = waiting {
            let _ = tx.send(error);
        }
    }

    /// Stops waiting for `probe`.
    pub fn forget_probe(&self, probe: &str) {
        self.probes.lock().unwrap_or_else(PoisonError::into_inner).remove(probe);
    }

    /// Where to send `node` a feasibility query: its session here, if its agent answers them.
    #[must_use]
    pub fn asker(&self, node: Uuid) -> Option<Sender<ControlMessage>> {
        self.guard().get(&node).filter(|e| e.answers).map(|e| e.tx.clone())
    }

    /// Where `node`'s answer to `query` will arrive, once it has been sent it.
    pub fn expect_feasibility(&self, query: &str, node: Uuid) -> oneshot::Receiver<Feasible> {
        let (tx, rx) = oneshot::channel();
        let mut queries = self.queries.lock().unwrap_or_else(PoisonError::into_inner);
        queries.insert(query.to_owned(), (node, tx));
        rx
    }

    /// `node`'s answer to a feasibility query. An answer from any node but the one asked is
    /// dropped: a query's identifier is a secret between the control plane and that node.
    pub fn feasibility_result(&self, node: Uuid, r: &FeasibilityResult) {
        let mut queries = self.queries.lock().unwrap_or_else(PoisonError::into_inner);
        if queries.get(&r.query).is_none_or(|(asked, _)| *asked != node) {
            return;
        }
        let Some((_, tx)) = queries.remove(&r.query) else { return };
        let answer = if r.unanswered {
            Feasible::Unanswered
        } else if r.accepted {
            Feasible::Accepted
        } else {
            Feasible::Refused(r.detail.clone())
        };
        let _ = tx.send(answer);
    }

    /// Stops waiting for `query`.
    pub fn forget_feasibility(&self, query: &str) {
        self.queries.lock().unwrap_or_else(PoisonError::into_inner).remove(query);
    }

    /// The nodes with a session here.
    #[must_use]
    pub fn nodes(&self) -> Vec<Uuid> {
        self.guard().keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    fn result(query: &str) -> FeasibilityResult {
        FeasibilityResult { query: query.to_owned(), accepted: true, ..Default::default() }
    }

    #[test]
    fn a_feasibility_answer_is_taken_from_the_node_asked_alone() {
        let sessions = Sessions::default();
        let (asked, other) = (Uuid::now_v7(), Uuid::now_v7());
        let mut answered = sessions.expect_feasibility("q1", asked);
        sessions.feasibility_result(other, &result("q1"));
        assert!(answered.try_recv().is_err(), "another node's answer is dropped");
        sessions.feasibility_result(asked, &result("q1"));
        assert_eq!(answered.try_recv().unwrap(), Feasible::Accepted);
        let mut silent = sessions.expect_feasibility("q2", asked);
        sessions.forget_feasibility("q2");
        sessions.feasibility_result(asked, &result("q2"));
        assert!(silent.try_recv().is_err(), "a forgotten query takes no answer");
    }
}
