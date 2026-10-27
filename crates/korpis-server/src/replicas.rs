// SPDX-License-Identifier: AGPL-3.0-or-later

//! What each node is told of replicas (§2.2 of docs/06-storage.md), as this replica tells the
//! nodes it holds sessions with. Each replica recomputes it every [`EVERY`] from the replicas the
//! control plane holds, and publishes it when it changed; each session sends its node its own
//! part once one is computed, empty when the node keeps nothing, and again whenever it changes,
//! so a transfer made due or a receive recorded reaches both nodes within about [`EVERY`]. What a
//! node is told is the whole of what it keeps, so a node is always told, even nothing.

use crate::api::Api;
use crate::sessions::Sessions;
use korpis_core::workload::transfers::NodeReplicas;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_wire::replica::replicas_to_pb;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio::sync::watch;
use uuid::Uuid;

/// How often what the nodes are told is recomputed.
pub const EVERY: Duration = Duration::from_secs(2);

/// What every node is told, by node.
type Told = BTreeMap<Uuid, NodeReplicas>;

/// What the nodes are told as last published; none until the first computation.
#[derive(Debug)]
pub struct KeptReplicas {
    tx: watch::Sender<Option<Arc<Told>>>,
}

impl Default for KeptReplicas {
    fn default() -> Self {
        Self { tx: watch::Sender::new(None) }
    }
}

impl KeptReplicas {
    /// What is published, and every change after.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<Told>>> {
        self.tx.subscribe()
    }

    /// Publishes `told` unless it is what is published; whether it did.
    #[must_use = "whether what the nodes are told changed"]
    pub fn publish(&self, told: Told) -> bool {
        self.tx.send_if_modified(|held| {
            if held.as_deref() == Some(&told) {
                return false;
            }
            *held = Some(Arc::new(told));
            true
        })
    }
}

/// Recomputes and publishes what the nodes are told every [`EVERY`]. The task ends with the
/// process.
pub(crate) async fn keep(api: Arc<Api>) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().replication().await {
            Ok(told) => {
                let nodes = told.len();
                if api.replicas().publish(told) {
                    tracing::debug!(nodes, "what the nodes are told of replicas changed");
                }
            }
            // Nodes keep what they were told; the next tick tries again.
            Err(e) => tracing::warn!(error = %e, "computing what nodes keep of replicas failed"),
        }
    }
}

/// Sends `node`, down its session `id`, its part of each `held` publishes, whenever that differs
/// from what it was told last, until the session ends or another replaces it.
pub(crate) async fn push(
    (sessions, node, id, mut ended): (Arc<Sessions>, Uuid, u64, watch::Receiver<bool>),
    tx: Sender<agent::ControlMessage>,
    mut held: watch::Receiver<Option<Arc<Told>>>,
) {
    let mut told: Option<agent::Replicas> = None;
    loop {
        let current = held.borrow_and_update().clone();
        let mine = current.map(|t| t.get(&node).map(replicas_to_pb).unwrap_or_default());
        if let Some(replicas) = mine.filter(|r| told.as_ref() != Some(r)) {
            let message = agent::ControlMessage {
                message: Some(Down::Replicas(Box::new(replicas.clone()))),
                ..Default::default()
            };
            if tx.send(message).await.is_err() {
                return;
            }
            told = Some(replicas);
        }
        tokio::select! {
            changed = held.changed() => if changed.is_err() { return },
            () = tx.closed() => return,
            _ = ended.wait_for(|e| *e) => return,
        }
        if !sessions.is_current(node, id) {
            return;
        }
    }
}
