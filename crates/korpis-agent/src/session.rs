// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control stream, as the agent holds it (§4.2 and §7 of docs/02-architecture.md). It dials
//! out, proves the node's identity in a signed Hello, keeps the heartbeat the Welcome declares,
//! believes an assignment set only once every signature verifies, keeps it on disk, and
//! acknowledges it. When the stream drops it dials again, backing off; the node keeps doing its
//! job meanwhile from the set it holds (§4.6).

use crate::held::{HeldSet, Refused};
use crate::state::{Identity, StateDir};
use crate::tls;
use buffa_types::google::protobuf::Timestamp;
use connectrpc::client::{BidiRecvHalf, ClientConfig, ClientTransport, HttpClient};
use korpis_core::node::identity::{hello_message, sign};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, watch};

/// The protocol this agent speaks.
pub const PROTOCOL: &str = "korpis.agent.v1";
/// The longest wait between dialling attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

type Recv = BidiRecvHalf<
    <HttpClient as ClientTransport>::ResponseBody,
    agent::__buffa::view::ControlMessageView<'static>,
>;

/// What the node reports about itself: in the Hello, and in every heartbeat.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// Its drivers, each with its capabilities.
    pub drivers: Vec<pb::DriverInfo>,
    /// Its storage classes.
    pub storage_classes: Vec<pb::StorageClassInfo>,
    /// Total.
    pub capacity: pb::Resources,
    /// What placements may reserve.
    pub allocatable: pb::Resources,
}

/// What happened on the session, for whoever watches it.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The Hello was accepted.
    Welcomed {
        /// The heartbeat interval the control plane declared.
        heartbeat: Duration,
    },
    /// A new set is held.
    Held {
        /// Its digest.
        digest: String,
    },
    /// The control plane said the set held is current.
    InSync,
    /// A set was refused.
    Refused(Refused),
    /// The stream ended, and why.
    Disconnected(String),
}

/// What the rest of the agent holds of the session.
#[derive(Debug)]
pub struct Link {
    /// The set held, as it changes.
    pub held: watch::Receiver<HeldSet>,
    /// Everything else the control plane sends: lease answers, journal acknowledgements.
    pub inbound: mpsc::Receiver<Down>,
    /// What to send up.
    pub outbound: mpsc::Sender<agent::AgentMessage>,
    /// What happens, for watching.
    pub events: broadcast::Sender<Event>,
}

/// The session.
pub struct Session {
    identity: Identity,
    state: StateDir,
    facts: Arc<dyn Fn() -> Facts + Send + Sync>,
    held: watch::Sender<HeldSet>,
    inbound: mpsc::Sender<Down>,
    outbound: mpsc::Receiver<agent::AgentMessage>,
    events: broadcast::Sender<Event>,
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
    /// A session for `identity`, holding the set kept in `state` if it still verifies, and
    /// reporting what `facts` says; and the link the rest of the agent uses.
    pub fn new(
        identity: Identity,
        state: StateDir,
        facts: Arc<dyn Fn() -> Facts + Send + Sync>,
    ) -> (Self, Link) {
        let kept = state.root().join("assignments.binpb");
        let initial = HeldSet::load(&kept, &identity.keys.assignment_keys, &identity.node)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "the kept assignment set does not verify; holding none");
                HeldSet::default()
            });
        let (held, held_rx) = watch::channel(initial);
        let (inbound, inbound_rx) = mpsc::channel(256);
        let (outbound_tx, outbound) = mpsc::channel(1024);
        let (events, _) = broadcast::channel(64);
        let link = Link {
            held: held_rx,
            inbound: inbound_rx,
            outbound: outbound_tx,
            events: events.clone(),
        };
        (Self { identity, state, facts, held, inbound, outbound, events }, link)
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

    fn hello(&self) -> Result<agent::AgentMessage, String> {
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
            set_digest: self.held.borrow().digest.clone(),
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

    fn heartbeat(&self) -> agent::AgentMessage {
        let facts = (self.facts)();
        let status = agent::NodeStatus {
            capacity: facts.capacity.into(),
            allocatable: facts.allocatable.into(),
            drivers: facts.drivers,
            ..Default::default()
        };
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
        send.send(self.hello()?).await.map_err(|e| e.to_string())?;
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
        loop {
            tokio::select! {
                next = rx.recv() => {
                    let next = next.ok_or("the reader stopped")??;
                    if let Some(ack) = self.receive(next).await {
                        send.send(ack).await.map_err(|e| e.to_string())?;
                    }
                }
                _ = tick.tick() => send.send(self.heartbeat()).await.map_err(|e| e.to_string())?,
                Some(up) = self.outbound.recv() => send.send(up).await.map_err(|e| e.to_string())?,
            }
        }
    }

    /// Handles one message; the acknowledgement to send, if it was a set.
    async fn receive(&self, m: agent::ControlMessage) -> Option<agent::AgentMessage> {
        match m.message? {
            Down::AssignmentSet(set) => self.hold(&set),
            Down::InSync(_) => {
                let _ = self.events.send(Event::InSync);
                None
            }
            other => {
                let _ = self.inbound.send(other).await;
                None
            }
        }
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
