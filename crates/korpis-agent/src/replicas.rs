// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's part in volumes' replication (§2.2 of docs/06-storage.md). The control plane tells
//! the node, in its `Replicas` message, the transfers from its volumes and the replicas it keeps;
//! the session keeps that on disk, so a transfer under way survives the control plane's absence
//! until its ticket expires. The agent sends each transfer once, and again every [`RETRY`] while
//! it fails and its ticket lasts, to the receiving node's mesh address (`send`); listens on its
//! own for what is sent to it, admitting a stream only under a ticket the token keys open that
//! names this node and a replica it was told it receives, from the mesh address of the node it
//! was told sends it (`receive`); journals how each receive ended; and releases what
//! replication no longer needs, of what it sent and of what it received (`release`).

mod receive;
mod release;
mod send;

use crate::host::Replicating;
use crate::journal::JournalHandle;
use crate::session::Event;
use korpis_core::authz::token::KeyRing;
use korpis_proto::korpis::agent::v1 as agent;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;
use uuid::Uuid;

/// The TCP port a node receives transfers on, at its own mesh address alone.
pub const TRANSFER_PORT: u16 = 51820;

/// How often what is due is looked at again: a failed transfer retried, what is no longer
/// needed released.
pub const RETRY: Duration = Duration::from_secs(10);

/// The file in the state directory that holds what the node was last told.
pub(crate) const TOLD: &str = "replicas.binpb";

/// The members of the mesh, as the overlay keeps them.
const PEERS: &str = "overlay-peers.binpb";

/// A transfer from this node: the volume, the node receiving it, and the snapshot.
type SendKey = (String, String, String);

/// Where a transfer from this node stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sending {
    /// Being sent now.
    Running,
    /// Written whole; the receiving node says how it ended.
    Sent,
    /// It failed at this time, and is sent again [`RETRY`] after.
    Failed(Instant),
}

/// What the tasks share.
#[derive(Debug, Default)]
struct Shared {
    /// What the node was last told; none before the first.
    told: Option<agent::Replicas>,
    /// The mesh's members, as last named.
    peers: agent::Peers,
    /// The transfers from this node, as they stand.
    sends: BTreeMap<SendKey, Sending>,
}

/// The node's part in replication.
#[derive(Debug)]
pub struct Replication<H> {
    host: H,
    /// This node.
    node: Uuid,
    /// Its own address in the mesh: where it receives, and what it sends from.
    mesh: Option<Ipv4Addr>,
    /// The token keys in force, which open tickets.
    keys: watch::Receiver<Arc<KeyRing>>,
    journal: JournalHandle,
    shared: Mutex<Shared>,
}

/// Reads a message kept at `path`; none where nothing is, or what is cannot be read.
fn kept<M: buffa::Message + Default>(path: &Path) -> Option<M> {
    let bytes = std::fs::read(path).ok()?;
    M::decode_from_slice(&bytes).ok()
}

/// The mesh address of `node` among `peers`: the last of its IPv4 pool.
pub(crate) fn mesh_of(peers: &agent::Peers, node: &str) -> Option<Ipv4Addr> {
    let p = peers.peers.iter().find(|p| p.node == node)?;
    let pools: Vec<ipnet::IpNet> = p.allowed_ips.iter().filter_map(|a| a.parse().ok()).collect();
    korpis_core::edge::mesh_address(&pools)
}

impl<H> Replication<H> {
    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl<H: Replicating + Clone> Replication<H> {
    /// The part of the node `node`, whose state is at `state`, with its mesh address `mesh`;
    /// opening tickets with `keys` and journaling into `journal`.
    #[must_use]
    pub fn new(
        host: H,
        (state, node, mesh): (&Path, Uuid, Option<Ipv4Addr>),
        keys: watch::Receiver<Arc<KeyRing>>,
        journal: JournalHandle,
    ) -> Arc<Self> {
        let shared = Shared {
            told: kept(&state.join(TOLD)),
            peers: kept(&state.join(PEERS)).unwrap_or_default(),
            sends: BTreeMap::new(),
        };
        Arc::new(Self { host, node, mesh, keys, journal, shared: Mutex::new(shared) })
    }

    /// Sends what is due, and releases what is no longer needed, every [`RETRY`] and whenever
    /// `events` say what the node is told or who the mesh's members are; receives on the mesh
    /// address. The task ends with the agent.
    pub async fn run(self: Arc<Self>, mut events: broadcast::Receiver<Event>) {
        if let Some(mesh) = self.mesh {
            tokio::spawn(receive::listen(Arc::clone(&self), mesh));
        }
        let mut tick = tokio::time::interval(RETRY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                e = events.recv() => match e {
                    Ok(Event::Replicas(r)) => self.shared().told = Some(r),
                    Ok(Event::Peers(p)) => self.shared().peers = p,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "replication missed session events");
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            }
            send::start_due(&self);
            release::release(&self).await;
        }
    }
}

/// Keeps `r` at `state`, as what the node was last told of replicas.
///
/// # Errors
/// The file could not be written.
pub(crate) fn keep_told(state: &Path, r: &agent::Replicas) -> std::io::Result<()> {
    crate::durable::replace(&state.join(TOLD), &buffa::Message::encode_to_vec(r))
}
