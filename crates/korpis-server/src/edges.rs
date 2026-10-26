// SPDX-License-Identifier: AGPL-3.0-or-later

//! What each node is told of edges (§3.1 of docs/07-networking.md), as this replica tells the
//! nodes it holds sessions with. Each replica recomputes every node's table every [`EVERY`] from
//! what the control plane holds, and publishes it when it changed; each session sends its node
//! its own table when it opens, and again whenever that changes, so a workload behind a group
//! that starts, stops, or moves reaches every member within about [`EVERY`].

use crate::api::Api;
use crate::sessions::Sessions;
use korpis_app::edge_group::EdgeTables;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_wire::edge::node_edges_to_pb;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio::sync::watch;
use uuid::Uuid;

/// How often every node's table is recomputed.
pub const EVERY: Duration = Duration::from_secs(2);

/// The tables as last published; none until the first computation.
#[derive(Debug)]
pub struct KeptEdges {
    tx: watch::Sender<Option<Arc<EdgeTables>>>,
}

impl Default for KeptEdges {
    fn default() -> Self {
        Self { tx: watch::Sender::new(None) }
    }
}

impl KeptEdges {
    /// The tables published, and every change after.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<EdgeTables>>> {
        self.tx.subscribe()
    }

    /// Publishes `tables` unless they are what is published; whether it did.
    #[must_use = "whether the tables changed"]
    pub fn publish(&self, tables: EdgeTables) -> bool {
        self.tx.send_if_modified(|held| {
            if held.as_deref() == Some(&tables) {
                return false;
            }
            *held = Some(Arc::new(tables));
            true
        })
    }
}

/// Recomputes and publishes every node's table every [`EVERY`]. The task ends with the process.
pub(crate) async fn keep(api: Arc<Api>) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().edge_tables().await {
            Ok(tables) => {
                let nodes = tables.0.len();
                if api.edges().publish(tables) {
                    tracing::debug!(nodes, "what the nodes are told of edges changed");
                }
            }
            // Nodes keep the tables they were told; the next tick tries again.
            Err(e) => tracing::warn!(error = %e, "computing the edges' tables failed"),
        }
    }
}

/// Sends `node`, down its session `id`, its table of each `held` publishes, whenever that differs
/// from what it was told last, until the session ends or another replaces it. A node no group
/// concerns is told nothing, and one that stops being concerned is told an empty table, so it
/// forgets what it held.
pub(crate) async fn push(
    (sessions, node, id, mut ended): (Arc<Sessions>, Uuid, u64, watch::Receiver<bool>),
    tx: Sender<agent::ControlMessage>,
    mut held: watch::Receiver<Option<Arc<EdgeTables>>>,
) {
    let mut told: Option<agent::Edges> = None;
    loop {
        let current = held.borrow_and_update().clone();
        let mine = current.and_then(|t| t.of(node).map(node_edges_to_pb));
        // Told something before and nothing now: told to forget it.
        let edges = mine.or_else(|| told.as_ref().map(|_| agent::Edges::default()));
        if let Some(edges) = edges.filter(|e| told.as_ref() != Some(e)) {
            let message = agent::ControlMessage {
                message: Some(Down::Edges(Box::new(edges.clone()))),
                ..Default::default()
            };
            if tx.send(message).await.is_err() {
                return;
            }
            told = Some(edges);
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
