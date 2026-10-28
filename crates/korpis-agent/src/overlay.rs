// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's side of the mesh (§9.1 of docs/07-networking.md). The control plane names the
//! mesh's other members; the agent keeps them on disk, so the mesh runs with the control plane
//! gone (§4.6 of docs/02-architecture.md), and has the helper keep the interface to them every
//! [`EVERY`] and whenever they change, admitting from the mesh only the dependents on other nodes
//! its assignments name. What the helper answers, its public key and each peer's latest
//! handshake, is what the node reports, from which the control plane judges which pairs reach
//! each other.

mod answer;
mod edges;
mod leases;
mod request;

use crate::durable;
use crate::held::HeldSet;
use crate::host::Host;
use crate::session::{Event, Facts};
use answer::Answering;
use ed25519_dalek::SigningKey;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use leases::{EdgeLeases, Holding};
use request::{admitted, facts, migrating_from, request};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;

/// What the node admits from the mesh by what it holds: each dependent's flow, and each node a
/// migration it receives is sent from.
type Admits = (Vec<helper::OverlayAdmitted>, Vec<String>);

/// What `held` admits.
fn admits(held: &HeldSet) -> Admits {
    (admitted(held), migrating_from(held))
}

/// The UDP port the mesh listens on unless `--overlay-port` names another.
pub const DEFAULT_PORT: u16 = 51820;

/// How often the interface is kept, and its handshakes read, without being told to.
pub const EVERY: Duration = Duration::from_secs(10);

/// The file in the state directory that holds the members last named.
const PEERS: &str = "overlay-peers.binpb";

/// The node's side of the mesh, kept by the helper.
#[derive(Debug)]
pub struct Overlay<H> {
    host: H,
    peers: PathBuf,
    edges: PathBuf,
    /// What the node was last told of replicas, which names the nodes sending to it.
    replicas: PathBuf,
    port: u16,
    pools: Vec<String>,
    advertised: Vec<String>,
    signer: (String, SigningKey),
    kept: watch::Sender<Option<agent::OverlayFacts>>,
    told: watch::Sender<agent::Edges>,
}

/// Sends each of `messages` up `outbound`; whether it is still open.
async fn sent(
    outbound: &mpsc::Sender<agent::AgentMessage>,
    messages: Vec<agent::AgentMessage>,
) -> bool {
    for m in messages {
        if outbound.send(m).await.is_err() {
            return false;
        }
    }
    true
}

/// Puts `now` in `was`; whether that changed it.
fn replaced(was: &mut agent::Edges, now: &agent::Edges) -> bool {
    let changed = was != now;
    if changed {
        was.clone_from(now);
    }
    changed
}

impl<H: Host> Overlay<H> {
    /// The mesh of the node whose state is at `state`, on `port`, its workloads' `pools` its
    /// allowed IPs, the node advertising `advertised` and answering edges' probes as `signer`,
    /// its wire identifier and key; and what it reports, as it changes.
    pub fn new(
        host: H,
        state: &std::path::Path,
        (port, pools): (u16, Vec<String>),
        (advertised, signer): (Vec<String>, (String, SigningKey)),
    ) -> (Self, watch::Receiver<Option<agent::OverlayFacts>>) {
        let (kept, reported) = watch::channel(None);
        let (peers, edges) = (state.join(PEERS), state.join(edges::EDGES));
        let replicas = state.join(crate::replicas::TOLD);
        let told = watch::Sender::new(agent::Edges::default());
        let o = Self { host, peers, edges, replicas, port, pools, advertised, signer, kept, told };
        (o, reported)
    }

    /// The table of edges last told, as it changes: what the node's proxy serves for its groups
    /// is read from it (§3.1 of docs/07-networking.md).
    #[must_use]
    pub fn edges(&self) -> watch::Receiver<agent::Edges> {
        self.told.subscribe()
    }

    /// The table of edges last told; none before the first.
    fn edges_told(&self) -> agent::Edges {
        let read = std::fs::read(&self.edges).ok();
        read.and_then(|b| buffa::Message::decode_from_slice(&b).ok()).unwrap_or_default()
    }

    /// Keeps the table of edges `e` on disk, for the next pass and the next start.
    fn told_edges(&self, e: &agent::Edges) {
        if let Err(e) = durable::replace(&self.edges, &buffa::Message::encode_to_vec(e)) {
            tracing::warn!(error = %e, "the edges' table could not be kept on disk");
        }
    }

    /// The members last named; none before the first.
    fn named(&self) -> agent::Peers {
        let read = std::fs::read(&self.peers).ok();
        read.and_then(|b| buffa::Message::decode_from_slice(&b).ok()).unwrap_or_default()
    }

    /// Keeps the interface to the members last named, admitting `admitted`, with the table of
    /// edges `told` and the addresses of `holding`, and publishes what the node reports; whether
    /// the helper kept it.
    async fn keep(
        &self,
        (admitted, migrating): Admits,
        (told, holding): (&agent::Edges, &Holding),
    ) -> bool {
        let named = self.named();
        let mut asked = request((self.port, &self.advertised), (&named, told, holding), admitted);
        self.transfers((&named, &migrating), &mut asked);
        match self.host.keep_overlay(&asked).await {
            Ok(kept) => {
                let reported = facts(self.port, &self.pools, kept);
                self.kept.send_if_modified(|held| {
                    let changed = held.as_ref() != Some(&reported);
                    *held = Some(reported);
                    changed
                });
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "the helper did not keep the mesh");
                false
            }
        }
    }

    /// Keeps the members `p` names on disk, for the next pass and the next start.
    fn told(&self, p: &agent::Peers) {
        if let Err(e) = durable::replace(&self.peers, &buffa::Message::encode_to_vec(p)) {
            tracing::warn!(error = %e, "the mesh's members could not be kept on disk");
        }
        if !p.outside.is_empty() {
            tracing::warn!(why = %p.outside, "this node is outside the mesh");
        }
    }

    /// Waits for what calls for the interface to be kept again: [`EVERY`] passing, `events`
    /// naming the members anew, what `held` admits changing from `admitted`, or an edge address
    /// held changing as `leases` say, asking on `outbound` for each lease as it falls due; none
    /// once the agent stops.
    async fn wake(
        &self,
        (events, held): (&mut broadcast::Receiver<Event>, &mut watch::Receiver<HeldSet>),
        admitted: &Admits,
        (leases, outbound): (&mut EdgeLeases, &mpsc::Sender<agent::AgentMessage>),
    ) -> Option<()> {
        let deadline = Instant::now() + EVERY;
        let told = self.edges_told();
        loop {
            let now = Instant::now();
            if !sent(outbound, leases.due(&told, now)).await {
                return None;
            }
            if leases.changed(&told, now) {
                return Some(());
            }
            let next =
                leases.next_change().filter(|t| *t > now).map_or(deadline, |t| t.min(deadline));
            tokio::select! {
                () = tokio::time::sleep_until(next) => {
                    if Instant::now() >= deadline {
                        return Some(());
                    }
                }
                changed = held.changed() => {
                    changed.ok()?;
                    if admits(&held.borrow()) != *admitted {
                        return Some(());
                    }
                }
                e = events.recv() => match e {
                    Ok(Event::Peers(p)) => {
                        self.told(&p);
                        return Some(());
                    }
                    Ok(Event::Edges(e)) => {
                        self.told_edges(&e);
                        return Some(());
                    }
                    // The session kept it on disk before passing it on.
                    Ok(Event::Replicas(_)) => return Some(()),
                    Ok(e @ (Event::EdgeLeaseGrant(_) | Event::EdgeLeaseRefusal(_))) => {
                        leases.answer(&e, Instant::now());
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return None,
                },
            }
        }
    }

    /// Keeps the mesh every [`EVERY`], and at once whenever `events` names its members anew,
    /// `held` changes what the node admits, or an edge address's lease is granted or runs out;
    /// asking for those leases, and saying each address released, on `outbound`; and answers the
    /// probes edges forward here.
    pub async fn run(
        self,
        (mut events, mut held): (broadcast::Receiver<Event>, watch::Receiver<HeldSet>),
        outbound: mpsc::Sender<agent::AgentMessage>,
    ) {
        let mut leases = EdgeLeases::default();
        let mut answering = Answering::new(self.signer.clone());
        loop {
            let admitted = admits(&held.borrow_and_update());
            let told = self.edges_told();
            self.told.send_if_modified(|was| replaced(was, &told));
            let holding = leases.holding(&told, Instant::now());
            let kept = self.keep(admitted.clone(), (&told, &holding)).await;
            if kept {
                answering.keep(&told).await;
            }
            let released = if kept { leases.kept(holding) } else { Vec::new() };
            if !sent(&outbound, released).await {
                return;
            }
            let waited = self.wake((&mut events, &mut held), &admitted, (&mut leases, &outbound));
            if waited.await.is_none() {
                return;
            }
        }
    }
}

impl<H: Host + Send + Sync + 'static> Overlay<H> {
    /// Runs it on the runtime, told its members by what `events` carries from now on, and what
    /// it admits by the set `held` holds, sending up `outbound`.
    pub fn spawn(
        self,
        (events, held): (&broadcast::Sender<Event>, watch::Receiver<HeldSet>),
        outbound: mpsc::Sender<agent::AgentMessage>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.run((events.subscribe(), held), outbound))
    }
}

/// `facts`, with the node's side of the mesh as `reported` last says.
#[must_use]
pub fn with_overlay(
    facts: Arc<dyn Fn() -> Facts + Send + Sync>,
    reported: watch::Receiver<Option<agent::OverlayFacts>>,
) -> Arc<dyn Fn() -> Facts + Send + Sync> {
    Arc::new(move || Facts { overlay: reported.borrow().clone(), ..facts() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use korpis_proto::korpis::v1 as pb;

    #[test]
    fn the_helper_is_asked_for_every_member_and_its_answer_reported_with_the_node_s_pools() {
        let named = agent::Peers {
            peers: vec![agent::Peer {
                node: "nd_1".into(),
                public_key: vec![2; 32],
                endpoint: "203.0.113.2:51820".into(),
                allowed_ips: vec!["10.48.4.0/22".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let none = (&named, &agent::Edges::default(), &Holding::new());
        let asked = request((51820, &[]), none, Vec::new());
        assert_eq!((asked.port, asked.peers.len()), (51820, 1));
        assert_eq!(asked.peers[0].allowed_ips, ["10.48.4.0/22"]);
        let answer = helper::KeepOverlayResponse {
            public_key: vec![1; 32],
            handshakes: vec![helper::PeerHandshake {
                public_key: vec![2; 32],
                latest_unix_seconds: 100,
                ..Default::default()
            }],
            ..Default::default()
        };
        let reported = facts(51820, &["10.48.0.0/22".into()], answer);
        assert_eq!(reported.pools, ["10.48.0.0/22"]);
        assert_eq!(reported.handshakes[0].latest_unix_seconds, 100);
    }

    #[test]
    fn each_dependent_on_another_node_is_admitted_once_over_its_endpoint_s_transport() {
        let dependent = |port: u32, protocol: pb::EndpointProtocol| agent::OverlayDependent {
            workload: "wl_web".into(),
            address: "10.48.4.2".into(),
            target_address: "10.48.0.2".into(),
            container_port: port,
            protocol: protocol.into(),
            ..Default::default()
        };
        let db = agent::Assignment {
            dependents: vec![
                dependent(5432, pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP),
                dependent(5432, pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP),
                dependent(53, pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP),
            ],
            ..Default::default()
        };
        let mut held = HeldSet::default();
        held.assignments.insert("wl_db".into(), db);
        let listed = admitted(&held);
        assert_eq!(listed.len(), 2, "the same dependent and port once");
        let udp = listed.iter().find(|a| a.port == 53).unwrap();
        assert_eq!(udp.transport.as_known(), Some(helper::Transport::TRANSPORT_UDP));
        assert_eq!((udp.source.as_str(), udp.destination.as_str()), ("10.48.4.2", "10.48.0.2"));
    }

    #[test]
    fn a_migration_received_admits_its_source_and_one_sent_admits_nothing() {
        use agent::MigrationSide as S;
        let role = |side: S, peer: &str| agent::Assignment {
            migration: agent::MigrationRole {
                role: side.into(),
                peer: peer.into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        let mut held = HeldSet::default();
        held.assignments.insert("wl_in".into(), role(S::MIGRATION_SIDE_DESTINATION, "node_a"));
        held.assignments.insert("wl_out".into(), role(S::MIGRATION_SIDE_SOURCE, "node_b"));
        assert_eq!(migrating_from(&held), ["node_a"]);
    }
}
