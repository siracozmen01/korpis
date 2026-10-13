// SPDX-License-Identifier: AGPL-3.0-or-later

//! The agent sessions this process holds: one per node, so what the control plane decides for a
//! node can be pushed down the stream that node opened (§4.2 of docs/02-architecture.md). A new
//! session for a node replaces the old one, whose outbound stream then ends: an agent that
//! reconnected after a network change must not leave a half-open twin receiving its
//! assignments.

use korpis_proto::korpis::agent::v1::ControlMessage;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::Notify;
use tokio::sync::mpsc::Sender;
use uuid::Uuid;

struct Entry {
    id: u64,
    tx: Sender<ControlMessage>,
    wake: Arc<Notify>,
}

/// The registry.
#[derive(Default)]
pub struct Sessions {
    open: Mutex<HashMap<Uuid, Entry>>,
    next: AtomicU64,
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

    /// Registers the session of `node` sending through `tx`, replacing any other; its id, and
    /// what wakes it to recompute the node's set.
    pub fn register(&self, node: Uuid, tx: Sender<ControlMessage>) -> (u64, Arc<Notify>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let wake = Arc::new(Notify::new());
        self.guard().insert(node, Entry { id, tx, wake: Arc::clone(&wake) });
        (id, wake)
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
        if open.get(&node).is_some_and(|e| e.id == id) {
            open.remove(&node);
        }
    }

    /// Where to send to `node`, if it has a session here.
    #[must_use]
    pub fn sender(&self, node: Uuid) -> Option<Sender<ControlMessage>> {
        self.guard().get(&node).map(|e| e.tx.clone())
    }

    /// The nodes with a session here.
    #[must_use]
    pub fn nodes(&self) -> Vec<Uuid> {
        self.guard().keys().copied().collect()
    }
}
