// SPDX-License-Identifier: AGPL-3.0-or-later

//! The mesh between nodes (§9.1 of docs/07-networking.md) as this replica tells it to the nodes
//! it holds sessions with. Each replica recomputes it every [`EVERY`] from what every node last
//! reported, and publishes it when it changed; each session sends its node the other members
//! when it opens, and again whenever what that node is told changes, so a node enrolled, removed,
//! or reporting a new key reaches every connected member within about [`EVERY`]. A node that
//! reports no mesh interface is told nothing.

use crate::api::Api;
use crate::sessions::Sessions;
use korpis_core::node::mesh::Mesh;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_wire::node::peers_to_pb;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio::sync::watch;
use uuid::Uuid;

/// How often the mesh is recomputed.
pub const EVERY: Duration = Duration::from_secs(2);

/// The mesh as last published; none until the first computation.
#[derive(Debug)]
pub struct KeptMesh {
    tx: watch::Sender<Option<Arc<Mesh>>>,
}

impl Default for KeptMesh {
    fn default() -> Self {
        Self { tx: watch::Sender::new(None) }
    }
}

impl KeptMesh {
    /// The mesh published, and every change after.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<Mesh>>> {
        self.tx.subscribe()
    }

    /// Publishes `mesh` unless it is what is published; whether it did.
    #[must_use = "whether the mesh changed"]
    pub fn publish(&self, mesh: Mesh) -> bool {
        self.tx.send_if_modified(|held| {
            if held.as_deref() == Some(&mesh) {
                return false;
            }
            *held = Some(Arc::new(mesh));
            true
        })
    }
}

/// Recomputes and publishes the mesh every [`EVERY`]. The task ends with the process.
pub(crate) async fn keep(api: Arc<Api>) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().mesh().await {
            Ok(mesh) => {
                let (members, outside) = (mesh.members.len(), mesh.outside.len());
                if api.mesh().publish(mesh) {
                    tracing::info!(members, outside, "the mesh between nodes changed");
                    // Who is a member decides which dependencies cross nodes and what each
                    // node admits, so every node's set is computed again.
                    api.sessions().wake_all();
                }
            }
            // Nodes keep the peers they were told; the next tick tries again.
            Err(e) => tracing::warn!(error = %e, "computing the mesh failed"),
        }
    }
}

/// Sends `node`, down its session `id`, what it is told of each mesh `held` publishes, whenever
/// that differs from what it was told last, until the session ends or another replaces it.
pub(crate) async fn push(
    (sessions, node, id, mut ended): (Arc<Sessions>, Uuid, u64, watch::Receiver<bool>),
    tx: Sender<agent::ControlMessage>,
    mut held: watch::Receiver<Option<Arc<Mesh>>>,
) {
    let mut told: Option<agent::Peers> = None;
    loop {
        let current = held.borrow_and_update().clone();
        let peers = current.and_then(|mesh| peers_to_pb(&mesh, node));
        if let Some(peers) = peers.filter(|p| told.as_ref() != Some(p)) {
            let message = agent::ControlMessage {
                message: Some(Down::Peers(Box::new(peers.clone()))),
                ..Default::default()
            };
            if tx.send(message).await.is_err() {
                return;
            }
            told = Some(peers);
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

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_core::node::mesh::Member;

    #[test]
    fn the_mesh_is_published_only_when_it_changes() {
        let kept = KeptMesh::default();
        assert!(kept.publish(Mesh::default()), "the first is always published");
        assert!(!kept.publish(Mesh::default()));
        let member = Member {
            node: Uuid::from_u128(1),
            key: [1; 32],
            endpoint: None,
            pools: vec![],
            lineage: vec![],
        };
        assert!(kept.publish(Mesh { members: vec![member], outside: vec![] }));
    }
}
