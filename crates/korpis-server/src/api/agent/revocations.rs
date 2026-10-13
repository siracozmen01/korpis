// SPDX-License-Identifier: AGPL-3.0-or-later

//! The revocation list down one session (§6.3 of docs/08-identity.md): the current list when the
//! session opens, then every list published after it, until the session ends or another replaces
//! it.

use crate::sessions::Sessions;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tokio::sync::watch;
use uuid::Uuid;

type Lists = watch::Receiver<Option<Arc<agent::RevocationList>>>;

/// Sends each list down session `id` of `node`.
pub(super) async fn push(
    sessions: Arc<Sessions>,
    node: Uuid,
    id: u64,
    tx: Sender<agent::ControlMessage>,
    mut lists: Lists,
) {
    loop {
        let current = lists.borrow_and_update().clone();
        if let Some(list) = current {
            let message = agent::ControlMessage {
                message: Some(Down::RevocationList(Box::new((*list).clone()))),
                ..Default::default()
            };
            if tx.send(message).await.is_err() {
                return;
            }
        }
        tokio::select! {
            changed = lists.changed() => if changed.is_err() { return },
            () = tx.closed() => return,
        }
        if !sessions.is_current(node, id) {
            return;
        }
    }
}
