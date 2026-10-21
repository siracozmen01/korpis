// SPDX-License-Identifier: AGPL-3.0-or-later

//! The agent, assembled: the control stream, the journal and its maintenance, the lease keeper,
//! and the reconciler, over one runtime driver and the node's helper (§6 of
//! docs/02-architecture.md). It starts whether or not the control plane is reachable, because a
//! node keeps doing its job from the set it holds (§4.6), and a clean stop writes what the
//! journal still holds in memory before the process exits.

use crate::advertise::Advertise;
use crate::data::console::{self, ConsoleRead};
use crate::data::console_write::{self, ConsoleWrite};
use crate::data::files::{self, FileSessions};
use crate::data::handler::Handlers;
use crate::data::{DataLink, Judged};
use crate::host::Host;
use crate::identity::{ControlPlane, IdentitySockets};
use crate::journal::{Bounds, JournalHandle};
use crate::lease::{Keeper, Leases};
use crate::meter::Metering;
use crate::network::Pools;
use crate::offload::Offloader;
use crate::reconcile::streams::Streams;
use crate::reconcile::{Reconciler, Wiring};
use crate::runtime::{Runtime, RuntimeError};
use crate::session::{Event, Facts, Link, Session};
use crate::state::{StateDir, StateError};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

mod kept;

use kept::Kept;

/// Why the agent could not start.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The node has not joined, or its state does not read.
    #[error(transparent)]
    State(#[from] StateError),
    /// The journal or the reconciler's progress could not be opened.
    #[error("{what}: {source}")]
    Io {
        /// Which.
        what: &'static str,
        /// What failed.
        source: std::io::Error,
    },
    /// The driver could not be asked what it is.
    #[error("the driver: {0}")]
    Driver(#[from] RuntimeError),
}

/// What the node keeps back from placement for its own system, agent, and runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reserve {
    /// Millicores.
    pub cpu_millis: u64,
    /// Bytes.
    pub memory_bytes: u64,
}

impl Default for Reserve {
    fn default() -> Self {
        Self { cpu_millis: 500, memory_bytes: 1 << 30 }
    }
}

/// How the agent runs.
#[derive(Debug, Clone)]
pub struct Options {
    /// How often the reconciler looks without being told to.
    pub period: Duration,
    /// How long a runtime object is given to stop once signalled, before it is killed.
    pub stop_grace: Duration,
    /// How often the journal is maintained.
    pub maintain_every: Duration,
    /// The journal's bound.
    pub journal: Bounds,
    /// Where the workloads' identity sockets are made, one directory each; `identity` in the
    /// state directory when unset. The driver must be given the same root.
    pub identity_root: Option<std::path::PathBuf>,
    /// What serves each action of the sessions clients open through a relay (§3.1 of
    /// docs/02-architecture.md), beside `workload.console.read`, which the agent always serves;
    /// an action with none is refused.
    pub handlers: Handlers,
    /// The pools the node's workloads get addresses from, when it gives them networks (§4.1 of
    /// docs/07-networking.md); none gives them loopback alone.
    pub network: Option<Pools>,
    /// Runs the node's ingress proxy, setting this when it holds the ingress's ports (§3.2 of
    /// docs/07-networking.md); none runs no ingress. It needs the node to give its workloads
    /// networks and to advertise an address.
    pub ingress: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The meter's interval and where it reads interfaces (§2 of docs/15-observability.md).
    pub metering: Metering,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            period: Duration::from_secs(2),
            stop_grace: korpis_core::workload::plan::STOP_GRACE,
            maintain_every: Duration::from_secs(5),
            journal: Bounds::default(),
            identity_root: None,
            handlers: Handlers::default(),
            network: None,
            ingress: None,
            metering: Metering::default(),
        }
    }
}

/// Total memory, from `/proc/meminfo`; zero where it cannot be read.
fn memory_total() -> u64 {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    text.lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
        .map_or(0, |kib| kib.saturating_mul(1024))
}

/// This machine's capacity, and what placement may reserve of it: capacity less `reserve`,
/// each part of which is capped at a quarter of capacity so a small machine is not reserved away
/// (§2.1 of docs/05-scheduling.md).
#[must_use]
pub fn machine(reserve: Reserve) -> (pb::Resources, pb::Resources) {
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let cpu = u64::try_from(cores).unwrap_or(1).saturating_mul(1000);
    let memory = memory_total();
    let capacity = pb::Resources { cpu_millis: cpu, memory_bytes: memory, ..Default::default() };
    let allocatable = pb::Resources {
        cpu_millis: cpu.saturating_sub(reserve.cpu_millis.min(cpu / 4)),
        memory_bytes: memory.saturating_sub(reserve.memory_bytes.min(memory / 4)),
        ..Default::default()
    };
    (capacity, allocatable)
}

/// The facts a node reports: its driver, as the driver declares itself, its storage, as the
/// helper declares it, and this machine. A helper that cannot be reached leaves the node
/// offering no storage class rather than keeping it from starting.
pub async fn facts_of<R: Runtime, H: Host>(
    runtime: &R,
    host: &H,
    reserve: Reserve,
    advertise: Advertise,
) -> Result<Arc<dyn Fn() -> Facts + Send + Sync>, RuntimeError> {
    let driver = runtime.describe().await?;
    let storage_classes = host.storage().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "the helper did not declare its storage; offering none");
        Vec::new()
    });
    let (mut capacity, mut allocatable) = machine(reserve);
    // Disk is what the storage classes hold, and what is free in them now: anything already
    // on their filesystems, this node's own volumes included, counts as taken (§2.1 of
    // docs/05-scheduling.md).
    capacity.disk_bytes = storage_classes.iter().map(|c| c.capacity_bytes).sum();
    allocatable.disk_bytes = storage_classes.iter().map(|c| c.free_bytes).sum();
    let advertised = agent::Advertised::default();
    let facts = Facts { drivers: vec![driver], storage_classes, capacity, allocatable, advertised };
    Ok(Arc::new(move || Facts { advertised: advertise.current(), ..facts.clone() }))
}

/// What the node's ingress proxy is fed from and the task that runs it, where the node runs one:
/// it needs workloads to have networks, which the proxy reaches them on, and an address it
/// advertises, which the proxy listens on.
type Ingress = (
    Option<tokio::sync::watch::Sender<korpis_proto::korpis::ingress::v1::IngressConfig>>,
    Option<JoinHandle<()>>,
);

fn ingress<H: Host + Clone>(
    state: &StateDir,
    host: &H,
    (facts, serving): (Facts, Option<Arc<std::sync::atomic::AtomicBool>>),
    (networks, link): (bool, &Link),
) -> Ingress {
    let addresses = facts.advertised.addresses;
    let Some(serving) = serving.filter(|_| networks && !addresses.is_empty()) else {
        return (None, None);
    };
    let config = korpis_proto::korpis::ingress::v1::IngressConfig::default();
    let (publish, config) = tokio::sync::watch::channel(config);
    let control = state.root().join("ingress.sock");
    let watched = (serving, Arc::clone(&link.offered));
    let supervisor = crate::ingress::Supervisor::new(
        host.clone(),
        addresses,
        control,
        (config, link.held.clone()),
        watched,
    );
    (Some(publish), Some(tokio::spawn(supervisor.run())))
}

/// The workloads' identity sockets, under `root` or `identity` in the state directory, answered
/// through `credentials` and reaching `control_plane` (§9.5 of docs/08-identity.md).
fn identity_sockets(
    state: &StateDir,
    root: Option<std::path::PathBuf>,
    credentials: &Arc<crate::identity::Credentials>,
    control_plane: ControlPlane,
) -> Arc<IdentitySockets> {
    let root = root.unwrap_or_else(|| state.root().join("identity"));
    IdentitySockets::new(root, Arc::clone(credentials), control_plane)
}

/// What offloads the consoles in `streams`, its positions kept in the state directory.
fn offloader(
    state: &StateDir,
    streams: &Streams,
    (journal, jobs): (&JournalHandle, &Arc<crate::jobs::JobAccesses>),
    period: Duration,
) -> Result<Offloader, RunError> {
    let positions = state.root().join("offloaded");
    Offloader::new(&positions, streams.located(), (journal.clone(), Arc::clone(jobs)), period)
        .map_err(|source| RunError::Io { what: "the streams offloaded", source })
}

/// The data link of the node `identity` names, judging tokens by what `link` holds and serving
/// `handlers`.
fn data_link(identity: crate::state::Identity, link: &Link, handlers: Handlers) -> DataLink {
    let judged = Judged {
        keys: link.token_ring.clone(),
        revoked: link.revoked.clone(),
        held: link.held.clone(),
        handlers,
    };
    DataLink::new(identity, judged, link.relays.clone())
}

/// The running agent.
#[derive(Debug)]
pub struct Agent {
    tasks: Vec<JoinHandle<()>>,
    journal: JournalHandle,
    events: broadcast::Sender<Event>,
}

/// The data path's handlers, with the console's own: reading the streams the node holds, and
/// writing to the runtime object each workload runs as.
fn consoles<R: Runtime + Clone>(
    handlers: Handlers,
    streams: &Streams,
    (runtime, runtimes): (&R, &tokio::sync::watch::Sender<BTreeMap<String, String>>),
    journal: &JournalHandle,
) -> Handlers {
    let write = ConsoleWrite::new(runtime.clone(), runtimes.subscribe(), journal.clone());
    handlers
        .with(console::ACTION, Arc::new(ConsoleRead::new(streams.located(), journal.clone())))
        .with(console_write::ACTION, Arc::new(write))
}

impl Agent {
    /// Starts the agent of the node that joined into `state`, driving `runtime` and `host`;
    /// called inside a Tokio runtime, whose tasks it spawns.
    pub fn start<R: Runtime + Clone, H: Host + Clone>(
        state: &StateDir,
        runtime: R,
        host: H,
        facts: Arc<dyn Fn() -> Facts + Send + Sync>,
        options: Options,
    ) -> Result<Self, RunError> {
        let identity = state.load()?;
        let Kept { journal, progress, streams, inventory, network } =
            Kept::open(state, options.journal, options.network)?;
        let node = identity.node.clone();
        let control_plane =
            ControlPlane { url: identity.server.clone(), ca_pem: identity.ca_pem.clone() };
        let data = identity.clone();
        let ingress_facts = facts();
        let (session, link) = Session::new(identity, state.clone(), facts, journal.clone());
        let metered = (host.clone(), link.held.clone(), link.metering.clone(), journal.clone());
        let (runtime, sampler) =
            crate::meter::start(state.root(), runtime, metered, options.metering)
                .map_err(|source| RunError::Io { what: "the meter", source })?;
        let (ingress, proxy) =
            ingress(state, &host, (ingress_facts, options.ingress), (network.is_some(), &link));
        let answerer = crate::feasibility::answerer(runtime.clone());
        let session = session.answering(answerer, link.outbound.clone());
        let (runtimes, _) = tokio::sync::watch::channel(std::collections::BTreeMap::new());
        let handlers = consoles(options.handlers, &streams, (&runtime, &runtimes), &journal);
        let files = Arc::new(FileSessions::new(host.clone(), link.held.clone(), journal.clone()));
        let handlers = handlers.with(files::READ, files.clone()).with(files::WRITE, files);
        let data = data_link(data, &link, handlers);
        let leases = Leases::new(journal.clone());
        let keeper =
            Keeper::new(leases.clone(), link.held.clone(), link.inbound, link.outbound.clone());
        let wiring = Wiring {
            node,
            held: link.held.clone(),
            leases,
            journal: journal.clone(),
            outbound: link.outbound.clone(),
            events: link.events.subscribe(),
            period: options.period,
            grace: options.stop_grace,
            runtimes,
            jobs: link.jobs.clone(),
            steps: link.steps.clone(),
            inventory,
            network,
            ingress,
            certificates: link.certificates.clone(),
            identity: Some(identity_sockets(
                state,
                options.identity_root,
                &link.credentials,
                control_plane,
            )),
        };
        let offloader = offloader(state, &streams, (&journal, &link.jobs), options.period)?;
        let tasks = [
            tokio::spawn(session.run()),
            tokio::spawn(keeper.run()),
            journal.maintain_every(options.maintain_every),
            tokio::spawn(Reconciler::new(runtime, host, wiring, progress, streams).run()),
            tokio::spawn(data.run()),
            tokio::spawn(offloader.run()),
            tokio::spawn(sampler.run()),
        ];
        let tasks = tasks.into_iter().chain(proxy).collect();
        Ok(Self { tasks, journal, events: link.events })
    }

    /// What happens on the control stream, from now on.
    #[must_use]
    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// Stops the agent, leaving every workload as it is, and writes what the journal still
    /// counts in memory.
    pub async fn stop(self) {
        for t in &self.tasks {
            t.abort();
        }
        if let Err(e) = self.journal.close().await {
            tracing::error!(error = %e, "the journal could not be closed cleanly");
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserve_never_takes_more_than_a_quarter_of_the_machine() {
        let (capacity, allocatable) = machine(Reserve::default());
        assert!(capacity.cpu_millis >= 1000);
        assert!(allocatable.cpu_millis <= capacity.cpu_millis);
        let greedy = Reserve { cpu_millis: u64::MAX, memory_bytes: u64::MAX };
        let (capacity, allocatable) = machine(greedy);
        assert!(allocatable.cpu_millis >= capacity.cpu_millis / 4 * 3);
        assert!(allocatable.memory_bytes >= capacity.memory_bytes / 4 * 3);
    }
}
