// SPDX-License-Identifier: AGPL-3.0-or-later

//! What this process publishes, down one session: the current value when the session opens,
//! then every value published after it, until the session ends or another replaces it. The
//! revocation list (§6.3 of docs/08-identity.md) and the relays named (§3.1 of
//! docs/02-architecture.md) both reach a node this way.

use crate::sessions::Sessions;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tokio::sync::watch;
use uuid::Uuid;

/// Sends each value `held` publishes down session `id` of `node`, as `wrap` makes it a message.
pub(super) async fn push<T: Clone + Send + Sync>(
    (sessions, node, id): (Arc<Sessions>, Uuid, u64),
    tx: Sender<agent::ControlMessage>,
    mut held: watch::Receiver<Option<Arc<T>>>,
    wrap: fn(Box<T>) -> Down,
) {
    loop {
        let current = held.borrow_and_update().clone();
        if let Some(value) = current {
            let message = agent::ControlMessage {
                message: Some(wrap(Box::new((*value).clone()))),
                ..Default::default()
            };
            if tx.send(message).await.is_err() {
                return;
            }
        }
        tokio::select! {
            changed = held.changed() => if changed.is_err() { return },
            () = tx.closed() => return,
        }
        if !sessions.is_current(node, id) {
            return;
        }
    }
}
