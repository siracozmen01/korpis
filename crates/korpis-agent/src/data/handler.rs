// SPDX-License-Identifier: AGPL-3.0-or-later

//! What runs a session on the node, chosen by the action the session's token carries (§3.1 of
//! docs/02-architecture.md). A handler is given what the client opened with, already verified,
//! and the session's bytes both ways; the session ends when the handler returns, with the reason
//! it gives. An action no handler serves is refused as `unimplemented`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::mpsc;
use uuid::Uuid;

/// A session the node accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    /// The relay's identifier for it.
    pub session: String,
    /// The workload, as the wire names it.
    pub workload: String,
    /// The action.
    pub action: String,
    /// What the action needs to start, in its own message.
    pub params: Vec<u8>,
    /// Who opened it: the token's subject, which effects name.
    pub subject: String,
    /// The grant chain the token was minted from, leaf first.
    pub chain: Vec<Uuid>,
    /// When the token stops carrying the session unless renewed.
    pub expires_at: SystemTime,
}

/// The session's bytes: what the client sent, and where to send to it. The client is done when
/// `incoming` ends.
#[derive(Debug)]
pub struct Io {
    /// From the client.
    pub incoming: mpsc::Receiver<Vec<u8>>,
    /// To the client.
    pub outgoing: mpsc::Sender<Vec<u8>>,
}

/// Why a session ended other than normally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ended {
    /// A short code the client can branch on.
    pub code: String,
    /// For a human.
    pub message: String,
}

/// A session being served.
pub type Serving = Pin<Box<dyn Future<Output = Result<(), Ended>> + Send>>;

/// Serves one action's sessions.
pub trait Handler: Send + Sync {
    /// Serves `opened` over `io` until it is done.
    fn serve(&self, opened: Opened, io: Io) -> Serving;
}

/// The handlers this node runs, by action.
#[derive(Clone, Default)]
pub struct Handlers {
    by_action: HashMap<String, Arc<dyn Handler>>,
}

impl std::fmt::Debug for Handlers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.by_action.keys()).finish()
    }
}

impl Handlers {
    /// These, and `handler` serving `action`.
    #[must_use]
    pub fn with(mut self, action: &str, handler: Arc<dyn Handler>) -> Self {
        self.by_action.insert(action.to_owned(), handler);
        self
    }

    /// The handler serving `action`.
    #[must_use]
    pub fn get(&self, action: &str) -> Option<Arc<dyn Handler>> {
        self.by_action.get(action).cloned()
    }
}
