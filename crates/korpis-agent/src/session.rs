// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control stream, as the agent holds it (§4.2 and §7 of docs/02-architecture.md). It dials
//! out, proves the node's identity in a signed Hello, keeps the heartbeat the Welcome declares,
//! believes an assignment set only once every signature verifies, keeps it on disk, and
//! acknowledges it. When the stream drops it dials again, backing off; the node keeps doing its
//! job meanwhile from the set it holds (§4.6).

use crate::feasibility::Answerer;
use crate::held::HeldSet;
use crate::identity::Credentials;
use crate::jobs::JobAccesses;
use crate::journal::{JournalHandle, Shipper};
use crate::revoked::{Revoked, token_ring};
use crate::state::{Identity, StateDir};
use crate::tls;
use buffa_types::google::protobuf::Timestamp;
use connectrpc::client::{BidiRecvHalf, ClientConfig, ClientTransport, HttpClient};
use korpis_core::node::identity::{hello_message, sign};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Semaphore, broadcast, mpsc, watch};

mod answer;
mod link;

pub use link::{Event, Facts, Link};

/// The protocol this agent speaks.
pub const PROTOCOL: &str = "korpis.agent.v1";
/// The longest wait between dialling attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

type Recv = BidiRecvHalf<
    <HttpClient as ClientTransport>::ResponseBody,
    agent::__buffa::view::ControlMessageView<'static>,
>;

/// The session.
pub struct Session {
    identity: Identity,
    state: StateDir,
    facts: Arc<dyn Fn() -> Facts + Send + Sync>,
    journal: JournalHandle,
    held: watch::Sender<HeldSet>,
    revoked: Revoked,
    relays: watch::Sender<Vec<String>>,
    inbound: mpsc::Sender<Down>,
    outbound: mpsc::Receiver<agent::AgentMessage>,
    events: broadcast::Sender<Event>,
    credentials: Arc<Credentials>,
    jobs: Arc<JobAccesses>,
    steps: Arc<crate::steps::StepAnswers>,
    /// What answers feasibility queries, when the agent drives a runtime that can (§2.1 of
    /// docs/05-scheduling.md); where answers go; and how many more may be answered at once.
    answering: Option<(Answerer, mpsc::Sender<agent::AgentMessage>, Arc<Semaphore>)>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("node", &self.identity.node).finish_non_exhaustive()
    }
}

fn message(m: Up) -> agent::AgentMessage {
    agent::AgentMessage { message: Some(m), ..Default::default() }
}

fn now() -> Timestamp {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    Timestamp {
        seconds: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(since.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    }
}

/// The journal's messages to send; a journal that cannot be read is reported, and the session
/// carries on, since the records stay on disk for the next attempt.
fn shipped(r: std::io::Result<Vec<agent::AgentMessage>>) -> Vec<agent::AgentMessage> {
    r.unwrap_or_else(|e| {
        tracing::error!(error = %e, "the journal could not be sent");
        Vec::new()
    })
}

/// Reads the stream into a channel, so the session's loop never cancels a read half done.
async fn read(mut recv: Recv, out: mpsc::Sender<Result<agent::ControlMessage, String>>) {
    loop {
        let next = match recv.message::<agent::ControlMessage>().await {
            Ok(Some(m)) => Ok(m.to_owned_message()),
            Ok(None) => Err("the control plane closed the stream".to_owned()),
            Err(e) => Err(e.to_string()),
        };
        let ended = next.is_err();
        if out.send(next).await.is_err() || ended {
            return;
        }
    }
}

impl Session {
    /// A session for `identity`, holding the set kept in `state` if it still verifies,
    /// reporting what `facts` says, and sending `journal`; and the link the rest of the agent
    /// uses.
    pub fn new(
        identity: Identity,
        state: StateDir,
        facts: Arc<dyn Fn() -> Facts + Send + Sync>,
        journal: JournalHandle,
    ) -> (Self, Link) {
        let kept = state.root().join("assignments.binpb");
        let initial = HeldSet::load(&kept, &identity.keys.assignment_keys, &identity.node)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "the kept assignment set does not verify; holding none");
                HeldSet::default()
            });
        let (held, held_rx) = watch::channel(initial);
        let ring = token_ring(&identity.keys.token_keys);
        let (revoked, revoked_rx) = Revoked::load(state.root().join("revoked.binpb"), ring);
        let (relays, relays_rx) = watch::channel(Vec::new());
        let (inbound, inbound_rx) = mpsc::channel(256);
        let (outbound_tx, outbound) = mpsc::channel(1024);
        let (events, _) = broadcast::channel(64);
        let credentials = Credentials::new(outbound_tx.clone());
        let jobs = JobAccesses::new(outbound_tx.clone());
        let steps = crate::steps::StepAnswers::new(outbound_tx.clone());
        let link = Link {
            held: held_rx,
            revoked: revoked_rx,
            relays: relays_rx,
            inbound: inbound_rx,
            outbound: outbound_tx,
            events: events.clone(),
            credentials: credentials.clone(),
            jobs: jobs.clone(),
            steps: steps.clone(),
        };
        let session = Self {
            identity,
            state,
            facts,
            journal,
            held,
            revoked,
            relays,
            inbound,
            outbound,
            events,
            credentials,
            jobs,
            steps,
            answering: None,
        };
        (session, link)
    }

    /// Holds the session for as long as the process runs, dialling again whenever it drops.
    pub async fn run(mut self) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let reason = match self.once().await {
                Ok(()) => "ended".to_owned(),
                Err(e) => e,
            };
            tracing::warn!(%reason, "control stream down; dialling again in {backoff:?}");
            let _ = self.events.send(Event::Disconnected(reason));
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    fn hello(&self, (head, tail): (u64, u64)) -> Result<agent::AgentMessage, String> {
        let mut nonce = [0u8; 16];
        getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
        let at = now();
        let nanos = u32::try_from(at.nanos).unwrap_or(0);
        let signed = hello_message(&self.identity.node, at.seconds, nanos, &nonce);
        let facts = (self.facts)();
        let hello = agent::Hello {
            node: self.identity.node.clone(),
            agent_version: korpis_core::build::describe("korpis-agent"),
            protocol: PROTOCOL.to_owned(),
            drivers: facts.drivers,
            storage_classes: facts.storage_classes,
            capacity: facts.capacity.into(),
            allocatable: facts.allocatable.into(),
            advertised: facts.advertised.into(),
            answers_feasibility: self.answers(),
            set_digest: self.held.borrow().digest.clone(),
            journal_head: head,
            journal_tail: tail,
            auth: agent::NodeAuth {
                timestamp: at.into(),
                nonce: nonce.to_vec(),
                signature: sign(&self.identity.key, &signed),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        Ok(message(Up::Hello(Box::new(hello))))
    }

    async fn heartbeat(&self) -> agent::AgentMessage {
        let facts = (self.facts)();
        let mut status = agent::NodeStatus {
            capacity: facts.capacity.into(),
            allocatable: facts.allocatable.into(),
            drivers: facts.drivers,
            advertised: facts.advertised.into(),
            ..Default::default()
        };
        match self.journal.usage().await {
            Ok(u) => {
                status.journal_bytes_used = u.used;
                status.journal_bytes_bound = u.bound;
                status.journal_horizon = u.horizon.map(korpis_wire::duration).into();
            }
            Err(e) => tracing::error!(error = %e, "the journal's usage could not be read"),
        }
        message(Up::NodeStatus(Box::new(status)))
    }

    /// One connection, until it drops.
    async fn once(&mut self) -> Result<(), String> {
        let config = tls::trusting(&self.identity.ca_pem).map_err(|e| e.to_string())?;
        let uri: http::Uri =
            self.identity.server.parse().map_err(|e: http::uri::InvalidUri| e.to_string())?;
        let client = agent::AgentServiceClient::new(
            HttpClient::with_tls(Arc::new(config)),
            ClientConfig::new(uri),
        );
        let stream = client.session().await.map_err(|e| e.to_string())?;
        let (mut send, recv) = stream.into_split();
        let marks = self.journal.marks().await.map_err(|e| e.to_string())?;
        send.send(self.hello(marks)?).await.map_err(|e| e.to_string())?;
        let (tx, mut rx) = mpsc::channel(64);
        let reader = tokio::spawn(read(recv, tx));
        let result = self.serve(&mut send, &mut rx).await;
        reader.abort();
        result
    }

    /// The Welcome, then everything after it.
    async fn serve(
        &mut self,
        send: &mut connectrpc::client::BidiSendHalf<agent::AgentMessage>,
        rx: &mut mpsc::Receiver<Result<agent::ControlMessage, String>>,
    ) -> Result<(), String> {
        let first = rx.recv().await.ok_or("the stream ended before the Welcome")??;
        let Some(Down::Welcome(welcome)) = first.message else {
            return Err("the first answer was not a Welcome".to_owned());
        };
        let interval = welcome
            .heartbeat_interval
            .as_option()
            .and_then(|d| korpis_wire::duration_from("heartbeat_interval", d).ok())
            .unwrap_or(Duration::from_secs(30));
        let _ = self.events.send(Event::Welcomed { heartbeat: interval });
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let (journal, mut shipper) = (self.journal.clone(), Shipper::new(self.journal.clone()));
        loop {
            let up = tokio::select! {
                next = rx.recv() => {
                    let next = next.ok_or("the reader stopped")??;
                    self.receive(next, &mut shipper).await
                }
                _ = tick.tick() => {
                    let mut up = vec![self.heartbeat().await];
                    up.extend(shipped(shipper.stalled().await));
                    up
                }
                () = journal.changed() => shipped(shipper.pump().await),
                Some(up) = self.outbound.recv() => vec![up],
            };
            for m in up {
                send.send(m).await.map_err(|e| e.to_string())?;
            }
        }
    }

    /// Handles one message; what to send in answer.
    async fn receive(
        &self,
        m: agent::ControlMessage,
        shipper: &mut Shipper,
    ) -> Vec<agent::AgentMessage> {
        match m.message {
            Some(Down::AssignmentSet(set)) => self.hold(&set).into_iter().collect(),
            Some(Down::InSync(_)) => {
                let _ = self.events.send(Event::InSync);
                Vec::new()
            }
            Some(Down::JournalAck(ack)) => shipped(shipper.acknowledged(ack.seq).await),
            Some(Down::RevocationList(list)) => {
                match self.revoked.receive(&list) {
                    Ok((version, entries)) => {
                        let _ = self.events.send(Event::Revocations { version, entries });
                    }
                    // The list held stays; the control plane sends the next one within seconds.
                    Err(e) => {
                        tracing::error!(error = %e, "revocation list refused: a security event");
                    }
                }
                Vec::new()
            }
            Some(Down::Relays(r)) => {
                self.name_relays(&r);
                Vec::new()
            }
            Some(Down::ReachabilityProbe(p)) => {
                let forwarded = self.held.borrow().forwarded();
                let me = (self.identity.node.as_str(), &self.identity.key);
                vec![crate::probe::begin(me, *p, &forwarded).await]
            }
            Some(Down::WorkloadCredential(answer)) => {
                self.credentials.answer(*answer);
                Vec::new()
            }
            Some(Down::JobAccess(answer)) => {
                self.jobs.answer(*answer);
                Vec::new()
            }
            Some(Down::StepResolution(answer)) => {
                self.steps.answer(*answer);
                Vec::new()
            }
            Some(Down::FeasibilityQuery(q)) => {
                self.answer(q);
                Vec::new()
            }
            Some(other) => {
                let _ = self.inbound.send(other).await;
                Vec::new()
            }
            None => Vec::new(),
        }
    }

    /// Keeps one data connection to each relay `r` names, the control plane's own address
    /// standing for its relay role.
    fn name_relays(&self, r: &agent::Relays) {
        let mut wanted = r.urls.clone();
        if r.control_plane {
            wanted.push(self.identity.server.clone());
        }
        wanted.sort();
        wanted.dedup();
        self.relays.send_if_modified(|held| {
            let changed = *held != wanted;
            if changed {
                held.clone_from(&wanted);
            }
            changed
        });
    }

    /// Believes, keeps, and acknowledges a set, or refuses it and keeps the one held.
    fn hold(&self, set: &agent::AssignmentSet) -> Option<agent::AgentMessage> {
        let keys = &self.identity.keys.assignment_keys;
        let verified = match HeldSet::verify(set, keys, &self.identity.node) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "assignment set refused: a security event");
                let _ = self.events.send(Event::Refused(e));
                return None;
            }
        };
        if let Err(e) = verified.save(&self.state.root().join("assignments.binpb")) {
            // Not kept, not acknowledged: the control plane sends it again on reconnect.
            tracing::error!(error = %e, "the assignment set could not be kept");
            return None;
        }
        let digest = verified.digest.clone();
        self.held.send_replace(verified);
        let _ = self.events.send(Event::Held { digest: digest.clone() });
        let ack = agent::Ack { set_digest: digest, ..Default::default() };
        Some(message(Up::Ack(Box::new(ack))))
    }
}
