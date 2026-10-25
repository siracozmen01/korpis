// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the session shares with the rest of the agent: the facts it reports, what happens on
//! it, and the link through which the rest of the agent reads what it holds and sends up.

use crate::held::{HeldSet, Refused};
use crate::identity::Credentials;
use crate::jobs::JobAccesses;
use korpis_core::authz::revocation::RevocationList;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};

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
    /// What it advertises for inbound traffic (§9 of docs/07-networking.md).
    pub advertised: agent::Advertised,
    /// Its devices, as its spec files describe them (§6.1 of docs/05-scheduling.md).
    pub devices: Vec<pb::Device>,
    /// What its spec files describe that it does not offer, and why.
    pub unoffered_devices: Vec<pb::UnofferedDevice>,
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
    /// A revocation list is held.
    Revocations {
        /// Its version.
        version: u64,
        /// How many grants it lists.
        entries: usize,
    },
    /// The stream ended, and why.
    Disconnected(String),
}

/// What the rest of the agent holds of the session.
#[derive(Debug)]
pub struct Link {
    /// The set held, as it changes.
    pub held: watch::Receiver<HeldSet>,
    /// The token keys in force, as the control plane last sent them: what capability tokens
    /// and revocation lists are verified with (§8 of docs/03-state.md).
    pub token_ring: watch::Receiver<Arc<korpis_core::authz::token::KeyRing>>,
    /// The revocation list held, as it changes: what token verification refuses (§6.3 of
    /// docs/08-identity.md).
    pub revoked: watch::Receiver<Arc<RevocationList>>,
    /// The relays the last `Relays` named, the control plane's address among them when it serves
    /// the relay role; none before the first.
    pub relays: watch::Receiver<Vec<String>>,
    /// Everything else the control plane sends, such as lease answers. The session keeps
    /// the journal's acknowledgements, since it sends the journal.
    pub inbound: mpsc::Receiver<Down>,
    /// What to send up.
    pub outbound: mpsc::Sender<agent::AgentMessage>,
    /// What happens, for watching.
    pub events: broadcast::Sender<Event>,
    /// Workload credential requests, sent up this session and answered down it (§9.5 of
    /// docs/08-identity.md).
    pub credentials: Arc<Credentials>,
    /// Backup and restore jobs' access requests, sent up this session and answered down it
    /// (§5.5 of docs/06-storage.md).
    pub jobs: Arc<JobAccesses>,
    /// Installs' provider-step questions, sent up this session and answered down it (§4 of
    /// docs/09-recipes.md).
    pub steps: Arc<crate::steps::StepAnswers>,
    /// Query probes' questions to their health providers, sent up this session and answered
    /// down it (§5 of docs/16-extensions.md).
    pub queries: Arc<crate::health::Queries>,
    /// Told when what the node offers changes, such as its ingress being served; the session
    /// reports it at once rather than at the next heartbeat, so placement learns of it.
    pub offered: Arc<tokio::sync::Notify>,
    /// The certificates the node's proxy serves with, asked for up this session and answered
    /// down it (§3.2 of docs/07-networking.md).
    pub certificates: Arc<crate::ingress::Certificates>,
    /// The metering interval the control plane declared in its last Welcome; none before one
    /// declared it (§2 of docs/15-observability.md).
    pub metering: watch::Receiver<Option<Duration>>,
}
