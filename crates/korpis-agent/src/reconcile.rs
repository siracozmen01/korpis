// SPDX-License-Identifier: AGPL-3.0-or-later

//! The reconciler (§4 of docs/02-architecture.md): it makes the node match the set it holds,
//! level-triggered, one workload at a time. When the agent starts it adopts every runtime object
//! its driver created, matching them to assignments by the labels they carry, and touches none
//! of them (§4.4); an object no assignment claims is reported as an orphan and never deleted.
//! Then, on every change and on a period, it executes the plan steps it was given in order up
//! to `through`, journals each completion, keeps the approved power state and cycle, restarts a
//! crashed workload with a backoff, and reports what it observes. It acts only under a lease
//! (§4.5): without one it changes nothing, and when another node holds the workload it stops it.

mod act;
mod backup;
mod content;
mod install;
mod jobs;
mod managed;
mod observe;
pub mod progress;
mod restore;
mod step;
pub mod streams;
pub(crate) mod tail;
mod volume;
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
    /// The workloads' identity sockets (§9.5 of docs/08-identity.md); none where the node
    /// gives its workloads no identity, as in tests of other things.
    pub identity: Option<Arc<IdentitySockets>>,
    /// Where it says which runtime object each workload runs as, for the console's input.
    pub runtimes: watch::Sender<BTreeMap<String, String>>,
    /// Where backup and restore jobs ask for their key and access.
    pub jobs: Arc<crate::jobs::JobAccesses>,
    /// Where installs ask what their provider steps resolved to.
    pub steps: Arc<crate::steps::StepAnswers>,
}

/// The reconciler.
#[derive(Debug)]
pub struct Reconciler<R, H> {
    runtime: R,
    host: H,
    wiring: Wiring,
    progress: Progress,
    streams: Streams,
    tracked: BTreeMap<String, Tracked>,
    orphans: BTreeSet<String>,
    /// When the helper's jobs were last looked at for ones nothing names.
    reaped: Option<Instant>,
}

/// A string value.
fn text(s: &str) -> pb::Value {
    pb::Value {
        kind: Some(pb::__buffa::oneof::value::Kind::StringValue(s.to_owned())),
        ..Default::default()
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
        Self { runtime, host, wiring, progress, streams, tracked, orphans, reaped: None }
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
        self.adopt().await;
        let mut tick = tokio::time::interval(self.wiring.period);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut reports = Some(self.runtime.changes());
        loop {
            self.pass().await;
            let leases = self.wiring.leases.clone();
            let open = tokio::select! {
                changed = self.wiring.held.changed() => changed.is_ok(),
                () = leases.changed() => true,
                _ = tick.tick() => true,
                () = reported(&mut reports) => true,
                e = self.wiring.events.recv() => self.heard(&e),
            };
            if !open {
                return;
            }
        }
    }

    /// A session event; whether the session is still there.
    fn heard(&mut self, e: &Result<Event, broadcast::error::RecvError>) -> bool {
        match e {
            Ok(Event::Welcomed { .. }) => {
                for t in self.tracked.values_mut() {
                    t.sent = None;
                }
                true
            }
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => true,
            Err(broadcast::error::RecvError::Closed) => false,
        }
    }

    /// Takes over every runtime object the driver created, touching none (§4.4 of
    /// docs/02-architecture.md); what no assignment claims is an orphan.
    async fn adopt(&mut self) {
        let objects = match self.runtime.recover().await {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(error = %e, "the driver could not enumerate its objects");
                return;
            }
        };
        let held = self.wiring.held.borrow().clone();
        for o in objects {
            let workload = o.labels.as_option().map(|l| l.workload.clone()).unwrap_or_default();
            let claimed = held.assignments.contains_key(&workload);
            if !claimed || self.tracked.contains_key(&workload) {
                self.orphan(&o.runtime_id, &workload).await;
                continue;
            }
            let running = o.status.as_option().is_some_and(act::running);
            let t = Tracked {
                runtime: Some(o.runtime_id.clone()),
                labels: o.labels.as_option().cloned(),
                status: o.status.as_option().cloned(),
                stopped_by_us: !running,
                running_since: running.then(Instant::now),
                ..Tracked::default()
            };
            self.tracked.insert(workload.clone(), t);
            // A restarted agent serves the socket again at the path the container mounts.
            if let Some(Err(e)) = self.wiring.identity.as_ref().map(|i| i.ensure(&workload)) {
                tracing::error!(%workload, error = %e, "an identity socket could not be served");
            }
            let mut e = effect(Some(&workload), "node.adopt", true);
            e.after.insert("runtime_id".into(), text(&o.runtime_id));
            e.after.insert("running".into(), text(if running { "true" } else { "false" }));
            self.record(e).await;
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

    /// Reports a runtime object no assignment claims, once: it is never deleted.
    async fn orphan(&mut self, runtime_id: &str, workload: &str) {
        if !self.orphans.insert(runtime_id.to_owned()) {
            return;
        }
        tracing::warn!(%runtime_id, %workload, "orphaned runtime object; left untouched");
        let mut e = effect(None, "node.orphan", true);
        e.after.insert("runtime_id".into(), text(runtime_id));
        e.after.insert("workload_label".into(), text(workload));
        self.record(e).await;
    }

    /// One pass over the set held, and over what it no longer claims.
    async fn pass(&mut self) {
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
