// SPDX-License-Identifier: AGPL-3.0-or-later

//! The reconciler (§4 of docs/02-architecture.md): it makes the node match the set it holds,
//! level-triggered, one workload at a time. When the agent starts it adopts every runtime object
//! its driver created, matching them to assignments by the labels they carry, and touches none
//! of them (§4.4); an object no assignment claims is reported as an orphan and never deleted.
//! Then, on every change and on a period, it executes the plan steps it was given in order up
//! to `through`, journals each completion, keeps the approved power state and cycle, restarts a
//! crashed workload with a backoff, and reports what it observes. Between passes it runs each
//! running workload's health probes, and passes again when a verdict changes (§5 of
//! docs/04-runtimes.md). It acts only under a lease (§4.5): without one it changes nothing, and
//! when another node holds the workload it stops it.

pub(crate) mod act;
pub(crate) use act::volume_id;
pub(crate) use volume::driver_of;
mod adopt;
mod apply;
mod backup;
mod content;
mod cores;
mod deps;
mod devices;
pub(crate) use devices::block_classes;
mod halt;
mod health;
mod install;
mod jobs;
mod kinds;
mod managed;
pub(crate) mod network;
mod observe;
mod policy;
pub mod progress;
mod restore;
mod runs;
mod step;
pub mod streams;
pub(crate) mod tail;
mod volume;

pub use kinds::DriverKinds;
mod workload;

use crate::held::HeldSet;
use crate::host::Host;
use crate::identity::IdentitySockets;
use crate::journal::JournalHandle;
use crate::lease::Leases;
use crate::runtime::Runtime;
use crate::session::Event;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use progress::Progress;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use streams::Streams;
use tokio::sync::{broadcast, mpsc, watch};

/// How long a failed enumeration of the driver's objects waits before the first retry; each
/// retry waits twice as long, up to [`RECOVER_LONGEST_WAIT`].
const RECOVER_FIRST_WAIT: Duration = Duration::from_millis(500);

/// The longest wait between enumerations of a driver that keeps failing.
const RECOVER_LONGEST_WAIT: Duration = Duration::from_secs(15);

/// What the reconciler knows of one workload's runtime object, and of what it did to it.
#[derive(Debug, Clone, Default)]
struct Tracked {
    /// The runtime object, if there is one.
    runtime: Option<String>,
    /// Its labels.
    labels: Option<driver::RuntimeLabels>,
    /// What the driver last said it was doing.
    status: Option<driver::RuntimeStatus>,
    /// Whether the reconciler stopped it, as opposed to it exiting by itself.
    stopped_by_us: bool,
    /// Why the driver refused to prepare the target, while it does.
    rejected: Option<driver::Rejection>,
    /// Crash restarts in a row, and when the next may be made.
    restarts: u32,
    next_restart: Option<Instant>,
    /// Since when it has been running, to forgive past crashes once it stays up.
    running_since: Option<Instant>,
    /// Since when the driver has said it cannot tell what it is doing, while it says so.
    unknown_since: Option<Instant>,
    /// The generation and power version it last acted on.
    generation: u64,
    power_version: u64,
    /// Whether its lease was released after its retirement.
    released: bool,
    /// Whether its supersession was reported.
    superseded: bool,
    /// The observation last sent, and when its state last changed.
    sent: Option<pb::Observation>,
    since: Option<SystemTime>,
    /// The required dependency it waits for before it is created, by name.
    waiting: Option<String>,
    /// A task or scheduled workload's runs, by run, each its own object (§4.2 of
    /// docs/05-scheduling.md).
    runs: BTreeMap<String, runs::RunTracked>,
    /// Where the stop of its object is, while one is under way.
    halting: halt::Halting,
    /// Its recipe's pre-stop hook, and the recipe it was read from (§3 of docs/09-recipes.md).
    pre_stop: Option<korpis_core::recipe::behaviour::Hook>,
    hook_of: String,
    /// Its health probes in the life of its object (§5 of docs/04-runtimes.md).
    probing: health::Probing,
    /// The kind of the liveness probe whose failure it is being restarted for, until it is.
    recovering: Option<&'static str>,
    /// The cgroup parent its object's dedicated cores are held under, and their logical CPUs
    /// (§5.2 of docs/05-scheduling.md).
    cores: (String, Vec<u32>),
}

/// Where the reconciler reads and writes.
#[derive(Debug)]
pub struct Wiring {
    /// The node it runs on.
    pub node: String,
    /// The set held, as it changes.
    pub held: watch::Receiver<HeldSet>,
    /// What it may do, by lease.
    pub leases: Leases,
    /// Where step completions and node events are recorded.
    pub journal: JournalHandle,
    /// Where observations go.
    pub outbound: mpsc::Sender<agent::AgentMessage>,
    /// The session's events: every new session is sent every observation again.
    pub events: broadcast::Receiver<Event>,
    /// How often it looks without being told to.
    pub period: Duration,
    /// How long a runtime object is given to stop once signalled, before it is killed.
    pub grace: Duration,
    /// The workloads' identity sockets (§9.5 of docs/08-identity.md); none where the node
    /// gives its workloads no identity, as in tests of other things.
    pub identity: Option<Arc<IdentitySockets>>,
    /// Where it says which runtime object each workload runs as, for the console's input.
    pub runtimes: watch::Sender<BTreeMap<String, String>>,
    /// Where backup and restore jobs ask for their key and access.
    pub jobs: Arc<crate::jobs::JobAccesses>,
    /// Where installs ask what their provider steps resolved to.
    pub steps: Arc<crate::steps::StepAnswers>,
    /// The recipes whose content the node holds, reported for content locality (§2.2 of
    /// docs/05-scheduling.md).
    pub inventory: crate::inventory::Inventory,
    /// The workloads' networks (§4.1 of docs/07-networking.md); none where the node gives its
    /// workloads loopback alone.
    pub network: Option<crate::network::Networking>,
    /// Where what the node's ingress proxy serves is published (§3.2 of docs/07-networking.md);
    /// none where the node runs no ingress.
    pub ingress: Option<watch::Sender<korpis_proto::korpis::ingress::v1::IngressConfig>>,
    /// The certificates its proxy serves with, asked of the control plane and kept.
    pub certificates: Arc<crate::ingress::Certificates>,
    /// The table of edges the mesh keeps, whose groups' routes the proxy serves too (§3.1 of
    /// docs/07-networking.md); none where the node keeps no mesh.
    pub edges: Option<watch::Receiver<agent::Edges>>,
    /// What each of the node's drivers is: whether it takes block devices, and holds a tap.
    pub driver_kinds: DriverKinds,
    /// Where query probes ask their health provider's rounds (§5 of docs/16-extensions.md).
    pub queries: Arc<crate::health::Queries>,
    /// The reading jobs live backup sessions hold, which are not forgotten under them.
    pub reading: crate::data::backup::Reading,
    /// The node's devices, which a placement's holds are given from; none where it offers none.
    pub devices: Option<crate::devices::Devices>,
    /// What the node offers its workloads beside its drivers.
    pub offered: Offered,
}

/// What a node offers its workloads beside its drivers, as it declared it.
#[derive(Debug, Clone, Default)]
pub struct Offered {
    /// The cores it offers for dedication, which a placement's are given from (§5.2 of
    /// docs/05-scheduling.md).
    pub cores: Vec<pb::Core>,
    /// Its block classes, whose volumes are made only with the device their assignment carries
    /// (§2.1 of docs/06-storage.md).
    pub block_classes: BTreeSet<String>,
}

/// The reconciler.
#[derive(Debug)]
pub struct Reconciler<R, H> {
    /// Shared with the tasks that run exec probes' commands.
    runtime: Arc<R>,
    host: H,
    wiring: Wiring,
    progress: Progress,
    streams: Streams,
    tracked: BTreeMap<String, Tracked>,
    orphans: BTreeSet<String>,
    /// When the helper's jobs were last looked at for ones nothing names.
    reaped: Option<Instant>,
    /// Where health probes' attempts report.
    outcomes: health::Outcomes,
    /// The CPUs the helper keeps out of every cpuset but their holders'.
    cores_kept: cores::Kept,
    /// What it last said the proxy holds of its edge groups' challenges, this session.
    proofs: Option<agent::EdgeChallengesHeld>,
}

/// A string value.
fn text(s: &str) -> pb::Value {
    pb::Value {
        kind: Some(pb::__buffa::oneof::value::Kind::StringValue(s.to_owned())),
        ..Default::default()
    }
}

/// Resolves when the table of edges changes; never, once nothing keeps it.
async fn edges_changed(edges: &mut Option<watch::Receiver<agent::Edges>>) {
    let Some(rx) = edges else { return std::future::pending().await };
    if rx.changed().await.is_err() {
        *edges = None;
    }
}

/// Resolves when the driver reports a runtime object changed; never, once it cannot.
async fn reported(reports: &mut Option<broadcast::Receiver<String>>) {
    let Some(rx) = reports else { return std::future::pending().await };
    match rx.recv().await {
        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
        Err(broadcast::error::RecvError::Closed) => *reports = None,
    }
}

/// Resolves at `at`; never, without one.
async fn until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// An effect the node records.
fn effect(workload: Option<&str>, action: &str, ok: bool) -> pb::Effect {
    let outcome = if ok {
        pb::EffectOutcome::EFFECT_OUTCOME_SUCCEEDED
    } else {
        pb::EffectOutcome::EFFECT_OUTCOME_FAILED
    };
    pb::Effect {
        workload: workload.map(str::to_owned),
        action: action.to_owned(),
        outcome: outcome.into(),
        ..Default::default()
    }
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// A reconciler driving `runtime` and `host`, keeping its progress in `progress` and how far
    /// it has journaled each stream in `streams`.
    #[must_use]
    pub fn new(runtime: R, host: H, wiring: Wiring, progress: Progress, streams: Streams) -> Self {
        let (tracked, orphans) = (BTreeMap::new(), BTreeSet::new());
        let outcomes = health::Outcomes::default();
        let runtime = Arc::new(runtime);
        let (reaped, cores_kept) = (None, cores::Kept::default());
        Self {
            runtime,
            host,
            wiring,
            progress,
            streams,
            tracked,
            orphans,
            reaped,
            outcomes,
            cores_kept,
            proofs: None,
        }
    }

    /// Records `e` in the journal; whether it is on disk.
    async fn record(&self, e: pb::Effect) -> bool {
        match self.wiring.journal.append(Body::Effect(Box::new(e))).await {
            Ok(_) => true,
            Err(e) => {
                tracing::error!(error = %e, "an effect could not be journalled");
                false
            }
        }
    }

    /// Adopts every object the driver created, then reconciles until the session ends.
    pub async fn run(mut self) {
        let Some(objects) = self.recovered().await else { return };
        self.adopt(objects).await;
        let mut tick = tokio::time::interval(self.wiring.period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut reports = Some(self.runtime.changes());
        let mut pass = true;
        loop {
            if pass {
                self.pass().await;
            }
            let leases = self.wiring.leases.clone();
            let due = self.probes_due();
            // A probe's attempt changes nothing until it changes a verdict, so it does not make
            // the pass every other wake does; a heartbeat's check is its verdict at once.
            pass = tokio::select! {
                changed = self.wiring.held.changed() => if changed.is_ok() { true } else { return },
                () = leases.changed() => true,
                _ = tick.tick() => true,
                () = reported(&mut reports) => true,
                e = self.wiring.events.recv() => if self.heard(&e) { true } else { return },
                () = edges_changed(&mut self.wiring.edges) => true,
                () = until(due) => self.probe().await,
                Some(o) = self.outcomes.rx.recv() => self.judged(&o),
            };
        }
    }

    /// A session event; whether the session is still there.
    fn heard(&mut self, e: &Result<Event, broadcast::error::RecvError>) -> bool {
        match e {
            Ok(Event::Welcomed { .. }) => {
                for t in self.tracked.values_mut() {
                    t.sent = None;
                }
                self.proofs = None;
                self.wiring.inventory.resend();
                true
            }
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => true,
            Err(broadcast::error::RecvError::Closed) => false,
        }
    }

    /// Forgets a workload that left the set with nothing left of it on the node.
    fn forget(&mut self, workload: &str) {
        self.tracked.remove(workload);
        if let Some(identity) = &self.wiring.identity {
            identity.remove(workload);
        }
        if let Err(e) = self.progress.forget(workload) {
            tracing::error!(error = %e, "progress could not be kept");
        }
    }

    /// One pass over the set held, and over what it no longer claims.
    async fn pass(&mut self) {
        if let Some(report) = self.wiring.inventory.report() {
            // A report lost with a session is sent again by the next one.
            let _ = self.wiring.outbound.send(report).await;
        }
        let held = self.wiring.held.borrow().clone();
        for a in held.assignments.values() {
            self.reconcile(a).await;
        }
        let gone: Vec<(String, Option<String>)> = self
            .tracked
            .iter()
            .filter(|(w, _)| !held.assignments.contains_key(*w))
            .map(|(w, t)| (w.clone(), t.runtime.clone()))
            .collect();
        for (workload, runtime) in gone {
            if let Some(id) = runtime {
                self.orphan(&id, &workload).await;
            } else {
                self.forget(&workload);
            }
        }
        self.release_networks(&held).await;
        self.release_devices(&held).await;
        self.keep_cores().await;
        self.serve_resolvers(&held).await;
        self.publish_ingress(&held).await;
        self.serve_relays(&held);
        self.refresh_dependency_files(&held);
        self.refresh_networks(&held).await;
        self.tail(&held).await;
        self.reap(&held).await;
        self.publish_runtimes();
    }

    /// Says which runtime object each workload runs as, when that changed.
    fn publish_runtimes(&self) {
        let now: BTreeMap<String, String> = self
            .tracked
            .iter()
            .filter_map(|(w, t)| t.runtime.clone().map(|r| (w.clone(), r)))
            .collect();
        self.wiring.runtimes.send_if_modified(|held| {
            let changed = *held != now;
            if changed {
                held.clone_from(&now);
            }
            changed
        });
    }
}
