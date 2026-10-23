// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading every source every ten seconds, and journaling a sample for every workload the node
//! holds at the end of each metering interval, by the node's clock (§2 of
//! docs/15-observability.md, "Measuring it, as designed").

use super::book::{self, Book};
use super::now::Readings;
use super::{Meter, READ_EVERY, reading};
use crate::held::HeldSet;
use crate::host::Host;
use crate::journal::JournalHandle;
use crate::reconcile::act::volume_id;
use crate::runtime::{Runtime, RuntimeError};
use korpis_core::workload::network::host_link;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::watch;

/// Memory unread for longer than this, an agent stopped between readings, is not integrated: it
/// was not measured, and the span is a loss.
const LONGEST_READ: Duration = Duration::from_secs(30);
/// Memory in use was not read for a span.
pub const MEMORY_UNREAD: &str = "memory unread";

/// What the meter reads.
#[derive(Debug, Clone)]
pub struct Sources<R, H> {
    /// The runtime, for each object's counters.
    pub runtime: R,
    /// The host, for each volume's stored bytes.
    pub host: H,
    /// The assignments the node holds: which workloads it samples, and their lease epochs.
    pub held: watch::Receiver<HeldSet>,
    /// Where the kernel lists network interfaces, `/sys/class/net` on a node.
    pub net: PathBuf,
    /// The interval the control plane declared, which is the one sampled at once it is told.
    pub told: watch::Receiver<Option<Duration>>,
}

/// The meter's reading and sampling.
#[derive(Debug)]
pub struct Sampler<R, H> {
    meter: Arc<Meter>,
    sources: Sources<R, H>,
    journal: JournalHandle,
    interval: Duration,
}

/// The start of the interval of length `interval` that `t` is in.
fn start_of(t: SystemTime, interval: Duration) -> SystemTime {
    let since = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let step = interval.as_secs().max(1);
    SystemTime::UNIX_EPOCH + Duration::from_secs(since.as_secs() / step * step)
}

fn counter(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

impl<R: Runtime, H: Host> Sampler<R, H> {
    /// Reads `sources` into `meter`, and journals into `journal` every `interval`.
    pub const fn new(
        meter: Arc<Meter>,
        sources: Sources<R, H>,
        journal: JournalHandle,
        interval: Duration,
    ) -> Self {
        Self { meter, sources, journal, interval }
    }

    /// Watches what each workload comes to at every reading.
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<Readings> {
        self.meter.watch()
    }

    /// The interval sampled at: the control plane's, once it declared one, or else the agent's.
    fn interval(&self) -> Duration {
        (*self.sources.told.borrow()).unwrap_or(self.interval)
    }

    /// Reads and samples until the task is dropped.
    pub async fn run(self) {
        let started = self.meter.with_book(|b| b.read_at()).unwrap_or_else(SystemTime::now);
        let mut current = start_of(started, self.interval());
        // An interval shorter than a reading's period is read at its own pace, the control
        // plane's once it declared one.
        let mut period = READ_EVERY.min(self.interval());
        let mut tick = tokio::time::interval(period);
        loop {
            tick.tick().await;
            self.read(SystemTime::now()).await;
            let now = start_of(SystemTime::now(), self.interval());
            if now > current {
                self.sample(current, now.duration_since(current).unwrap_or_default()).await;
                current = now;
            }
            let declared = READ_EVERY.min(self.interval());
            if declared != period {
                period = declared;
                tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            }
        }
    }

    /// Reads every object and every held workload's interface at `now`.
    pub async fn read(&self, now: SystemTime) {
        let last = self.meter.with_book(|b| b.read_at());
        let elapsed = last.and_then(|l| now.duration_since(l).ok()).unwrap_or(READ_EVERY);
        let integrated = elapsed.min(LONGEST_READ);
        let mut memory = BTreeMap::<String, u64>::new();
        for (object, workload) in self.meter.live() {
            if let Some(bytes) = self.read_object((&workload, &object), now, integrated).await {
                let held = memory.entry(workload).or_default();
                *held = held.saturating_add(bytes);
            }
        }
        let held: Vec<String> = self.sources.held.borrow().assignments.keys().cloned().collect();
        for workload in &held {
            self.read_link(workload, now);
        }
        self.read_table(now).await;
        self.meter.with_book(|b| {
            if elapsed > LONGEST_READ
                && let Some(from) = last
            {
                let every: std::collections::BTreeSet<String> =
                    held.iter().cloned().chain(b.workloads()).collect();
                let every: Vec<String> = every.into_iter().collect();
                b.lost_all(&every, (from, now), MEMORY_UNREAD);
            }
            b.read_all(now);
        });
        self.meter.keep();
        self.meter.publish(now, &memory);
    }

    /// Reads `workload`'s `object`, its memory integrated over `integrated`; the memory it
    /// holds, where that was read.
    async fn read_object(
        &self,
        (workload, object): (&str, &str),
        now: SystemTime,
        integrated: Duration,
    ) -> Option<u64> {
        let stats = match self.sources.runtime.stats(object).await {
            Ok(stats) => stats,
            Err(RuntimeError::NotFound(_)) => {
                self.meter.ended(object, None);
                return None;
            }
            Err(e) => {
                tracing::debug!(error = %e, object, "an object's counters went unread");
                return None;
            }
        };
        let c = stats.counters.as_option().cloned().unwrap_or_default();
        let memory = (c.memory_bytes, c.memory_peak_bytes);
        self.meter.with_book(|b| {
            b.read(workload, object, reading(&c), now);
            b.memory((workload, object), memory, integrated);
        });
        memory.0
    }

    /// Reads `workload`'s interface, if it has one.
    fn read_link(&self, workload: &str, now: SystemTime) {
        let stats = self.sources.net.join(host_link(workload)).join("statistics");
        // The host side receives what the workload sent, and sends what it receives.
        let (sent, received) = (counter(&stats.join("rx_bytes")), counter(&stats.join("tx_bytes")));
        let net = agent::MeterCounters {
            net_rx_bytes: received,
            net_tx_bytes: sent,
            ..Default::default()
        };
        self.meter.with_book(|b| match (sent, received) {
            (Some(_), Some(_)) => b.read(workload, "net", net, now),
            _ if b.has_source(workload, "net") => b.ended(workload, "net", None, now),
            _ => {}
        });
    }

    /// Reads what the host's table counted of every held workload's link, in one listing: its
    /// egress and its counted endpoints. A node whose helper lists nothing reads nothing, and a
    /// workload whose counters went with its network ends the source as its link's bytes do.
    async fn read_table(&self, now: SystemTime) {
        let held: Vec<(String, Vec<korpis_proto::korpis::v1::Endpoint>)> = {
            let held = self.sources.held.borrow();
            held.assignments.iter().map(|(w, a)| (w.clone(), a.endpoints.clone())).collect()
        };
        let listed = match self.sources.host.read_counters().await {
            Ok(r) => super::table::listed(r.counters),
            Err(e) => {
                tracing::debug!(error = %e, "the host's table went unread");
                return;
            }
        };
        let counted: Vec<_> = held
            .iter()
            .map(|(w, endpoints)| (w, super::table::counted(w, endpoints, &listed)))
            .collect();
        self.meter.with_book(|b| {
            for (workload, counted) in counted {
                b.read_or_end(workload, "table", counted, now);
            }
        });
    }

    /// The bytes `a`'s volumes hold, where their filesystem accounts them, and whether any of
    /// them counts a block device's allocated blocks rather than what its guest holds.
    async fn disk(&self, a: &agent::Assignment) -> (Option<u64>, bool) {
        let body = a.target.as_option().and_then(|t| t.body.as_option());
        let (mut total, mut allocated) = (None, false);
        for v in body.map(|b| b.volumes.as_slice()).unwrap_or_default() {
            match self.sources.host.volume_usage(&volume_id(a, &v.name)).await {
                Ok(u) if u.accounted => {
                    total = Some(total.unwrap_or(0_u64) + u.used_bytes);
                    allocated |= u.allocated;
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(error = %e, volume = v.name, "a volume went unread"),
            }
        }
        (total, allocated)
    }

    /// Journals the interval from `start`, `span` long, of every workload held or still read.
    pub async fn sample(&self, start: SystemTime, span: Duration) {
        let held = self.sources.held.borrow().assignments.clone();
        let mut workloads: Vec<String> = held.keys().cloned().collect();
        for w in self.meter.with_book(|b| b.workloads()) {
            if !workloads.contains(&w) {
                workloads.push(w);
            }
        }
        for workload in workloads {
            let a = held.get(&workload);
            let (disk, disk_allocated) = match a {
                Some(a) => self.disk(a).await,
                None => (None, false),
            };
            let closed = self.meter.with_book(|b| b.pending(&workload));
            let sample = agent::MeteringSample {
                workload: workload.clone(),
                interval_start: book::at(start).into(),
                interval: korpis_wire::duration(span).into(),
                lease_epoch: a.map_or(0, |a| a.lease_epoch),
                counters: closed.counters.into(),
                memory_peak_bytes: closed.peak,
                disk_bytes: disk,
                disk_allocated,
                losses: closed.losses,
                ..Default::default()
            };
            // Closed only once the journal holds it: a sample not journaled leaves its peak and
            // its losses to the next, and its counters are cumulative.
            match self.journal.append(Body::Metering(Box::new(sample))).await {
                Ok(_) => drop(self.meter.with_book(|b| b.close(&workload))),
                Err(e) => {
                    tracing::error!(error = %e, workload, "a metering sample went unjournaled");
                }
            }
            if a.is_none() {
                self.meter.with_book(|b: &mut Book| b.forget_if_idle(&workload));
            }
        }
        self.meter.keep();
    }
}
