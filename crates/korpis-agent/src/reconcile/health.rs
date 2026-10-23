// SPDX-License-Identifier: AGPL-3.0-or-later

//! A running workload's health probes, the node's half (Rule K-16, §5 of docs/04-runtimes.md). The
//! node runs the `tcp`, `http`, and `query` probes its intent declares, from sockets the helper
//! opens in the workload's namespace, each on its own period and from its initial delay after the
//! object started, never two attempts of one probe at once; a `heartbeat` probe is checked on its
//! period against when the workload last said on its identity socket that it is alive, and an
//! `exec` probe runs its command inside the object through its driver. A new start, or an object
//! built from another intent, is judged afresh. Its readiness decides whether its endpoints are
//! `ready` and whether a dependent waiting for it is created; its liveness, or its readiness where
//! it declares no liveness probe, is the health it reports; and a liveness probe that fails
//! restarts it, as a crash does, with the same backoff. A query attempt its health provider could
//! not judge leaves the verdict unknown (§5 of docs/16-extensions.md), as is an `exec` or a
//! `guest_agent` probe whose driver could not make it. A probe it declares that this node does not
//! run (a `heartbeat` where the node serves no identity sockets, or one from a socket where it
//! gives the workload no address) is unjudged: its health is unknown, and readiness is held to the
//! object running.

mod unsocketed;

use super::act::{Failure, running};
use super::runs::has_runs;
use super::workload::back_off;
use super::{Reconciler, Tracked, effect, text};
use crate::health::{Attempt, Query, Tried, attempt};
use crate::host::Host;
use crate::runtime::Runtime;
use korpis_core::workload::health::{Judge, Timing, Verdict, health};
use korpis_core::workload::observation::Health;
use korpis_core::workload::spec::health::{Probe, ProbeKind};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// Which of a workload's probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Which {
    Readiness,
    Liveness,
}

/// The life of a runtime object its probes judge: when it started, and the intent it was built
/// from.
type Life = (Instant, String);

/// What one attempt found.
#[derive(Debug)]
pub(super) struct Outcome {
    workload: String,
    life: Life,
    which: Which,
    tried: Tried,
}

/// One probe this node runs.
#[derive(Debug, Clone)]
struct Track {
    attempt: Attempt,
    timing: Timing,
    judge: Judge,
    /// When the next attempt is made.
    next: Instant,
    /// Whether an attempt is under way, which a query's rounds can make longer than a period.
    trying: bool,
}

/// One declared probe, as this node tends it.
#[derive(Debug, Clone, Default)]
enum Tended {
    #[default]
    Undeclared,
    /// Declared, and not run by this node.
    Elsewhere,
    Here(Track),
}

impl Tended {
    fn verdict(&self) -> Option<Verdict> {
        match self {
            Self::Undeclared => None,
            Self::Elsewhere => Some(Verdict::Unjudged),
            Self::Here(t) => Some(t.judge.verdict()),
        }
    }

    fn track(&mut self) -> Option<&mut Track> {
        match self {
            Self::Here(t) => Some(t),
            Self::Undeclared | Self::Elsewhere => None,
        }
    }
}

/// A workload's probes in one life of its runtime object; none while it does not run.
#[derive(Debug, Clone, Default)]
pub(super) struct Probing {
    life: Option<Life>,
    readiness: Tended,
    liveness: Tended,
}

impl Probing {
    /// Whether it can take traffic, given that it runs: its readiness probe passes, or this node
    /// runs none.
    pub(super) fn ready(&self) -> bool {
        match &self.readiness {
            Tended::Here(t) => t.judge.verdict() == Verdict::Passing,
            Tended::Undeclared | Tended::Elsewhere => true,
        }
    }

    /// Whether its liveness probe fails.
    pub(super) fn failing(&self) -> bool {
        self.liveness.verdict() == Some(Verdict::Failing)
    }

    /// The health its verdicts add up to.
    pub(super) fn health(&self) -> Health {
        health(self.liveness.verdict(), self.readiness.verdict())
    }

    fn track(&mut self, which: Which) -> Option<&mut Track> {
        match which {
            Which::Readiness => self.readiness.track(),
            Which::Liveness => self.liveness.track(),
        }
    }

    /// When each probe it runs and is not trying now is next attempted.
    fn due(&self) -> impl Iterator<Item = (Which, Instant)> + '_ {
        let of = |which, t: &Tended| match t {
            Tended::Here(t) if !t.trying => Some((which, t.next)),
            Tended::Here(_) | Tended::Undeclared | Tended::Elsewhere => None,
        };
        of(Which::Readiness, &self.readiness).into_iter().chain(of(Which::Liveness, &self.liveness))
    }
}

/// Where attempts report what they found.
#[derive(Debug)]
pub(super) struct Outcomes {
    tx: mpsc::Sender<Outcome>,
    pub(super) rx: mpsc::Receiver<Outcome>,
}

impl Default for Outcomes {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel(256);
        Self { tx, rx }
    }
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// What `a`'s probe `p` attempts from this node, if this node runs it: a `tcp`, `http`, or
    /// `query` probe, at the address the node gave the workload and the port of the endpoint it
    /// names, or a `heartbeat` where the node serves identity sockets. A query names the intent
    /// its object was built from, and whether it is the liveness probe, `asked`.
    fn attempt_of(&self, a: &agent::Assignment, p: &Probe, asked: (&str, bool)) -> Option<Attempt> {
        match &p.kind {
            ProbeKind::Heartbeat { max_interval } => {
                let heard = self.wiring.identity.is_some();
                return heard.then_some(Attempt::Heartbeat { max_interval: *max_interval });
            }
            ProbeKind::Exec { command } => return Some(Attempt::Exec { command: command.clone() }),
            ProbeKind::GuestAgent => return Some(Attempt::GuestAgent),
            _ => {}
        }
        let held = self.wiring.network.as_ref()?.addresses.of(&a.workload)?;
        let endpoint = |name: &str| a.endpoints.iter().find(|e| e.name == name);
        let at = |name: &str| {
            let e = endpoint(name)?;
            Some((held.ipv4, u16::try_from(e.container_port).ok()?).into())
        };
        match &p.kind {
            ProbeKind::Query { endpoint: name, .. } => {
                let protocol = endpoint(name)?.protocol.as_known();
                let udp = matches!(
                    protocol,
                    Some(
                        pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP
                            | pb::EndpointProtocol::ENDPOINT_PROTOCOL_QUIC
                    )
                );
                let (intent, liveness) = asked;
                Some(Attempt::Query(Query {
                    at: at(name)?,
                    udp,
                    workload: a.workload.clone(),
                    intent: intent.to_owned(),
                    liveness,
                }))
            }
            ProbeKind::Tcp { endpoint } => Some(Attempt::Tcp(at(endpoint)?)),
            ProbeKind::Http { endpoint, path, expect_status } => Some(Attempt::Http {
                at: at(endpoint)?,
                path: path.clone(),
                expect: *expect_status,
            }),
            _ => None,
        }
    }

    /// How this node tends `a`'s probe `p`, its liveness probe if `liveness`, in the life
    /// `life`.
    fn tend(
        &self,
        a: &agent::Assignment,
        p: Option<&Probe>,
        life: &Life,
        liveness: bool,
    ) -> Tended {
        let Some(p) = p else { return Tended::Undeclared };
        let (since, intent) = life;
        let Some(attempt) = self.attempt_of(a, p, (intent, liveness)) else {
            return Tended::Elsewhere;
        };
        let timing = Timing::of(p);
        let next = *since + timing.initial_delay;
        let judge = Judge::new(timing.failures);
        Tended::Here(Track { attempt, timing, judge, next, trying: false })
    }

    /// Aims `a`'s probes at the life its runtime object is in: kept while it is the same life,
    /// judged afresh in a new one, and none while it does not run.
    pub(super) fn aim(&self, a: &agent::Assignment, t: &mut Tracked) {
        let runs = t.status.as_ref().is_some_and(running) && !has_runs(a);
        let intent = t.labels.as_ref().map(|l| l.intent.clone());
        let life = t.running_since.filter(|_| runs).zip(intent);
        if t.probing.life == life {
            return;
        }
        let Some(life) = life else {
            t.probing = Probing::default();
            return;
        };
        let body = a.target.as_option().and_then(|i| i.body.as_option());
        let declared = body.and_then(|b| b.health.as_option());
        let h = korpis_wire::spec::health::health_from_pb("health", declared).unwrap_or_default();
        t.probing = Probing {
            readiness: self.tend(a, h.readiness.as_ref(), &life, false),
            liveness: self.tend(a, h.liveness.as_ref(), &life, true),
            life: Some(life),
        };
    }

    /// When the next attempt of any probe is due, if this node runs any.
    pub(super) fn probes_due(&self) -> Option<Instant> {
        self.tracked.values().flat_map(|t| t.probing.due()).map(|(_, at)| at).min()
    }

    /// Makes every attempt that is due, each in a task of its own that reports what it found,
    /// and checks every heartbeat that is due; whether a heartbeat changed a verdict.
    pub(super) async fn probe(&mut self) -> bool {
        let now = Instant::now();
        let mut changed = false;
        let due: Vec<(String, Which)> = self
            .tracked
            .iter()
            .flat_map(|(w, t)| t.probing.due().map(move |(which, at)| (w, which, at)))
            .filter(|(_, _, at)| *at <= now)
            .map(|(w, which, _)| (w.clone(), which))
            .collect();
        for (workload, which) in due {
            let track = self.tracked.get_mut(&workload).and_then(|t| t.probing.track(which));
            let what = track.map(|track| track.attempt.clone());
            match what {
                Some(Attempt::Heartbeat { max_interval }) => {
                    changed |= self.listened(&workload, which, max_interval, now);
                    continue;
                }
                Some(Attempt::Exec { command }) => {
                    self.run_exec(&workload, which, command, now);
                    continue;
                }
                Some(Attempt::GuestAgent) => {
                    self.ping_guest(&workload, which, now);
                    continue;
                }
                _ => {}
            }
            let udp = what.is_some_and(|a| a.udp());
            let socket = self.host.open_probe(&workload, udp).await;
            let Some(t) = self.tracked.get_mut(&workload) else { continue };
            let Some(life) = t.probing.life.clone() else { continue };
            let Some(track) = t.probing.track(which) else { continue };
            track.next = now + track.timing.period;
            let socket = match socket {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(%workload, error = %e.0, "a probe's socket could not be opened");
                    continue;
                }
            };
            track.trying = true;
            let (what, timeout, tx) = (track.attempt.clone(), track.timing.timeout, self.probed());
            let queries = Arc::clone(&self.wiring.queries);
            tokio::spawn(async move {
                let tried = attempt(socket, &what, timeout, &queries).await;
                // A reconciler that is gone has nothing left to judge.
                let _ = tx.send(Outcome { workload, life, which, tried }).await;
            });
        }
        changed
    }

    /// Where an attempt reports.
    fn probed(&self) -> mpsc::Sender<Outcome> {
        self.outcomes.tx.clone()
    }

    /// Records what an attempt found; whether a verdict changed, which a pass then acts on. An
    /// attempt at a life that has ended judges nothing.
    pub(super) fn judged(&mut self, o: &Outcome) -> bool {
        let Some(t) = self.tracked.get_mut(&o.workload) else { return false };
        if t.probing.life.as_ref() != Some(&o.life) {
            return false;
        }
        let Some(track) = t.probing.track(o.which) else { return false };
        track.trying = false;
        match o.tried {
            Tried::Passed => track.judge.record(true),
            Tried::Failed => track.judge.record(false),
            Tried::Unjudged => track.judge.unknown(),
        }
    }

    /// Restarts `a`'s workload when its liveness probe fails, as one that crashed is brought
    /// back: once its backoff allows, and counted with its crashes. `None` when it does not.
    pub(super) async fn unhealthy(
        &mut self,
        a: &agent::Assignment,
        t: &mut Tracked,
    ) -> Option<Result<(), Failure>> {
        let held_back = t.next_restart.is_some_and(|at| Instant::now() < at);
        if t.recovering.is_none() {
            if !t.probing.failing() || held_back {
                return None;
            }
            let Tended::Here(track) = &t.probing.liveness else { return None };
            t.recovering = Some(track.attempt.kind());
        }
        let result = self.restart(a, t).await;
        if matches!(result, Ok(None)) {
            // Still stopping: a later pass carries it on.
            return Some(Ok(()));
        }
        let probe = t.recovering.take().unwrap_or_default();
        back_off(t);
        let mut e = effect(Some(&a.workload), "node.restart", result.is_ok());
        e.after.insert("cause".into(), text("unhealthy"));
        e.after.insert("probe".into(), text(probe));
        self.record(e).await;
        Some(result.map(drop))
    }
}
