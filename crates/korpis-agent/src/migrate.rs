// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's part in a migration's copies (§8.1 of docs/05-scheduling.md). Each pass of a
//! volume is named by the control plane and carried, with its ticket, in both nodes' roles. The
//! source sends each pass its role names once, and again every [`RETRY`] while it fails and the
//! pass is still the one under way, to the destination's mesh address, and journals what it sent
//! (`send`). The destination receives on the transfer port, which hands it each stream whose
//! ticket is a migration pass's: admitted only under a ticket the token keys open, naming this
//! node, a pass its role names, from the peer its role names, at that peer's mesh address; it
//! journals that it holds the pass (`receive`). A volume of a class sending increments crosses
//! by the replica verbs, the pass its snapshot; any other by the tree verbs.

mod receive;
mod send;

use crate::held::HeldSet;
use crate::host::{Replicating, Trees};
use crate::journal::JournalHandle;
use crate::session::Event;
use korpis_core::authz::token::KeyRing;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::helper::v1 as helper;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;
use uuid::Uuid;

/// How often what is due is looked at again: a failed pass sent again.
pub const RETRY: Duration = Duration::from_secs(10);

/// A stream the transfer port read a migration pass's ticket from: the stream, the ticket, and
/// the address it came from.
pub type Incoming = (TcpStream, String, IpAddr);

/// Where a pass sent from this node stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sending {
    /// Being sent now.
    Running,
    /// Sent whole and reported.
    Sent,
    /// It failed at this time, and is sent again [`RETRY`] after.
    Failed(Instant),
}

/// What the tasks share.
#[derive(Debug, Default)]
struct Shared {
    /// The mesh's members, as last named.
    peers: agent::Peers,
    /// The passes sent from this node, by name.
    sends: BTreeMap<String, Sending>,
}

/// The node's part in migrations' copies.
#[derive(Debug)]
pub struct Migrating<H> {
    host: H,
    /// This node.
    node: Uuid,
    /// Its own address in the mesh: where it receives, and what it sends from.
    mesh: Option<Ipv4Addr>,
    /// The token keys in force, which open tickets.
    keys: watch::Receiver<Arc<KeyRing>>,
    journal: JournalHandle,
    held: watch::Receiver<HeldSet>,
    shared: Mutex<Shared>,
}

/// One volume's part in a pass, as a report carries it.
fn reported(v: &agent::MigrationVolumeRole, pass: &str) -> agent::MigrationVolumeProgress {
    agent::MigrationVolumeProgress {
        name: v.name.clone(),
        pass: pass.to_owned(),
        ..Default::default()
    }
}

impl<H> Migrating<H> {
    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Journals `side`'s report of `role`'s phase: going, with `volume`'s part, or failed for
    /// `why`.
    async fn journal(
        &self,
        role: &agent::MigrationRole,
        volume: agent::MigrationVolumeProgress,
        failed: Option<String>,
    ) {
        use agent::MigrationOutcomeOfPhase as O;
        let (outcome, detail) = match failed {
            Some(why) => (O::MIGRATION_OUTCOME_OF_PHASE_FAILED, why),
            None => (O::MIGRATION_OUTCOME_OF_PHASE_UNSPECIFIED, String::new()),
        };
        let p = agent::MigrationProgress {
            migration: role.migration.clone(),
            phase: role.phase,
            side: role.role,
            outcome: outcome.into(),
            detail,
            volumes: vec![volume],
            ..Default::default()
        };
        if let Err(e) = self.journal.append(Body::Migration(Box::new(p))).await {
            tracing::error!(error = %e, "a migration's report could not be journaled");
        }
    }
}

/// Joins the transfer connection `stream` to the helper's `socket` both ways until each side
/// closes. How the copy ended says nothing: a side closing first is how a pass ends, and what
/// the pass came to is the helper's to say.
async fn join(stream: TcpStream, socket: std::os::fd::OwnedFd) -> Result<(), String> {
    let socket = std::os::unix::net::UnixStream::from(socket);
    socket.set_nonblocking(true).map_err(|e| e.to_string())?;
    let mut socket = tokio::net::UnixStream::from_std(socket).map_err(|e| e.to_string())?;
    let mut stream = stream;
    if let Err(e) = tokio::io::copy_bidirectional(&mut stream, &mut socket).await {
        tracing::debug!(error = %e, "a pass's connection ended");
    }
    Ok(())
}

/// Waits, at most `within`, for the tree pass `asked` names to end, its stream having ended; the
/// bytes its sender sent, or why it failed.
async fn settled<H: Trees>(
    m: &Migrating<H>,
    asked: &helper::TreeStatusRequest,
    within: Duration,
) -> Result<u64, String> {
    use helper::TreeState as S;
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        let status = m.host.tree_status(asked).await.map_err(|e| e.to_string())?;
        match status.state.as_known() {
            Some(S::TREE_STATE_DONE) => return Ok(status.sent_bytes),
            Some(S::TREE_STATE_FAILED) => return Err(status.detail),
            Some(S::TREE_STATE_UNKNOWN) => return Err("the helper lost the pass".into()),
            _ => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
    Err("the pass did not end in time".into())
}

impl<H: Replicating + Trees + Clone> Migrating<H> {
    /// The part of the node `node`, whose state is at `state`, with its mesh address `mesh`,
    /// holding `held`; opening tickets with `keys` and journaling into `journal`.
    #[must_use]
    pub fn new(
        host: H,
        (state, node, mesh): (&Path, Uuid, Option<Ipv4Addr>),
        (keys, held): (watch::Receiver<Arc<KeyRing>>, watch::Receiver<HeldSet>),
        journal: JournalHandle,
    ) -> Arc<Self> {
        let peers = crate::replicas::kept_peers(state);
        let shared = Shared { peers, sends: BTreeMap::new() };
        Arc::new(Self { host, node, mesh, keys, journal, held, shared: Mutex::new(shared) })
    }

    /// Sends what is due every [`RETRY`] and whenever the set held changes, and receives each
    /// stream `incoming` hands it. The task ends with the agent.
    pub async fn run(
        self: Arc<Self>,
        mut events: broadcast::Receiver<Event>,
        mut incoming: mpsc::Receiver<Incoming>,
    ) {
        let mut held = self.held.clone();
        let mut tick = tokio::time::interval(RETRY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                changed = held.changed() => if changed.is_err() { return },
                stream = incoming.recv() => match stream {
                    Some(s) => { tokio::spawn(receive::handle(Arc::clone(&self), s)); continue }
                    None => return,
                },
                e = events.recv() => match e {
                    Ok(Event::Peers(p)) => self.shared().peers = p,
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            }
            send::start_due(&self);
        }
    }
}
