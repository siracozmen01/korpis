// SPDX-License-Identifier: AGPL-3.0-or-later

//! The relay role of this process (§3.1 of docs/02-architecture.md): the data connections nodes
//! attached here, and the sessions between a client opening one and the node carrying it.
//!
//! A client's open is verified, then the node it names is asked to open the session under a
//! random identifier; the node answers by carrying that identifier on a stream of its own, or by
//! refusing it. The identifier is good for one carry, and only the attached node was told it.
//! Nothing here is stored: a relay that restarts loses its sessions, and nodes attach again.

use korpis_core::authz::token::KeyRing;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, RwLock};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// How a node answered a session it was asked to open.
#[derive(Debug)]
pub enum Answer {
    /// It carries the session: where the client's frames go.
    Carried(mpsc::Sender<agent::CarryDown>),
    /// It will not.
    Refused {
        /// Why, in the codes of `OpenRefused`.
        code: String,
        /// For the client.
        message: String,
    },
}

/// A session asked of a node and not yet answered.
#[derive(Debug)]
pub struct Pending {
    /// Where the node's frames go.
    pub to_client: mpsc::Sender<pb::RelayResponse>,
    /// What the client is told first once the node carries the session, before any frame.
    pub opened: pb::RelayOpened,
    answer: oneshot::Sender<Answer>,
}

impl Pending {
    /// Gives the opener its answer; false if it stopped waiting.
    #[must_use = "whether the opener still waits"]
    pub fn answer(self, answer: Answer) -> bool {
        self.answer.send(answer).is_ok()
    }
}

struct Attachment {
    id: u64,
    tx: mpsc::Sender<agent::AttachDown>,
}

/// The role's state.
#[derive(Default)]
pub struct Relay {
    attached: Mutex<HashMap<Uuid, Attachment>>,
    pending: Mutex<HashMap<String, Pending>>,
    next: AtomicU64,
    keys: RwLock<KeyRing>,
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay")
            .field("attached", &lock(&self.attached).len())
            .field("pending", &lock(&self.pending).len())
            .finish_non_exhaustive()
    }
}

// A panic while a lock was held leaves a map that is still consistent: every operation inserts or
// removes one entry whole.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A fresh session identifier: 128 random bits, in hexadecimal.
fn session_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().fold(String::with_capacity(32), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

/// The token keys among `keys`, as the relay trusts them; a key it cannot read is skipped and
/// logged, and verifies nothing.
#[must_use]
pub fn token_ring(keys: &[korpis_app::ports::VerifyKey]) -> KeyRing {
    let mut ring = KeyRing::default();
    for k in keys {
        let Some(epoch) = korpis_core::authz::token::parse_key_name(&k.key_id) else { continue };
        if let Err(e) = ring.insert(epoch, &k.public) {
            tracing::warn!(key = %k.key_id, error = %e, "a token key is unusable");
        }
    }
    ring
}

impl Relay {
    /// Trusts `keys` for the tokens clients present.
    pub fn trust(&self, keys: KeyRing) {
        *self.keys.write().unwrap_or_else(PoisonError::into_inner) = keys;
    }

    /// The token keys trusted now.
    #[must_use]
    pub fn keys(&self) -> KeyRing {
        self.keys.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Records `node`'s data connection, sending through `tx`, replacing any other; its id.
    pub fn attach(&self, node: Uuid, tx: mpsc::Sender<agent::AttachDown>) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        lock(&self.attached).insert(node, Attachment { id, tx });
        id
    }

    /// Forgets data connection `id` of `node`, if it is still the current one.
    pub fn detach(&self, node: Uuid, id: u64) {
        let mut attached = lock(&self.attached);
        if attached.get(&node).is_some_and(|a| a.id == id) {
            attached.remove(&node);
        }
    }

    /// Asks `node` to open `open` (its session identifier is chosen here, and set in `opened`),
    /// the node's frames to go to `to_client`; the session identifier and where the answer
    /// arrives, or `None` if the node has no data connection here.
    pub async fn ask(
        &self,
        node: Uuid,
        mut open: agent::OpenSession,
        (to_client, mut opened): (mpsc::Sender<pb::RelayResponse>, pb::RelayOpened),
    ) -> Option<(String, oneshot::Receiver<Answer>)> {
        let tx = lock(&self.attached).get(&node).map(|a| a.tx.clone())?;
        open.session = session_id().ok()?;
        let session = open.session.clone();
        opened.session.clone_from(&session);
        let (answer, answered) = oneshot::channel();
        lock(&self.pending).insert(session.clone(), Pending { to_client, opened, answer });
        let message = agent::AttachDown {
            message: Some(agent::__buffa::oneof::attach_down::Message::Open(Box::new(open))),
            ..Default::default()
        };
        if tx.send(message).await.is_err() {
            self.forget(&session);
            return None;
        }
        Some((session, answered))
    }

    /// Takes the session the node is answering; `None` if it was never asked, already
    /// answered, or abandoned.
    pub fn take(&self, session: &str) -> Option<Pending> {
        lock(&self.pending).remove(session)
    }

    /// Abandons a session nobody answered.
    pub fn forget(&self, session: &str) {
        lock(&self.pending).remove(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_identifiers_are_long_and_distinct() {
        let (a, b) = (session_id().unwrap(), session_id().unwrap());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn a_node_is_asked_once_and_its_answer_reaches_the_opener() {
        let relay = Relay::default();
        let node = Uuid::now_v7();
        let (to_client, _) = mpsc::channel(1);
        let open = agent::OpenSession::default();
        let waiting = || (to_client.clone(), pb::RelayOpened::default());
        assert!(relay.ask(node, open.clone(), waiting()).await.is_none(), "not attached");

        let (tx, mut asked) = mpsc::channel(1);
        let first = relay.attach(node, tx);
        let (session, answered) = relay.ask(node, open, waiting()).await.unwrap();
        let Some(agent::__buffa::oneof::attach_down::Message::Open(sent)) =
            asked.recv().await.unwrap().message
        else {
            panic!("an open")
        };
        assert_eq!(sent.session, session);
        let pending = relay.take(&session).unwrap();
        assert_eq!(pending.opened.session, session);
        assert!(relay.take(&session).is_none(), "good for one carry");
        let refused = Answer::Refused { code: "not_here".into(), message: String::new() };
        assert!(pending.answer(refused));
        assert!(matches!(answered.await.unwrap(), Answer::Refused { .. }));

        let (tx, _) = mpsc::channel(1);
        let second = relay.attach(node, tx);
        relay.detach(node, first);
        assert!(lock(&relay.attached).get(&node).is_some_and(|a| a.id == second), "replaced");
    }
}
