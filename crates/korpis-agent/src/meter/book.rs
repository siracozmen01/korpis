// SPDX-License-Identifier: AGPL-3.0-or-later

//! The meter's accounts, apart from anything it reads them from (§2 of docs/15-observability.md,
//! "Measuring it, as designed"): each workload's counter is a base and the last reading of each
//! source still running, so a source that ends adds its last reading to the base and a workload's
//! counters run on across every object it is run as. A reading below the one before is a source
//! made again without the meter seeing it end, and what that source used since is a loss.

use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

/// A source whose final reading the meter could not take.
pub const ENDED_UNREAD: &str = "object ended unread";
/// A source made again without the meter seeing it end.
pub const COUNTER_RESET: &str = "counter reset";
/// The machine restarted, and every cgroup with it.
pub const NODE_REBOOTED: &str = "node rebooted";

fn sum(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
    }
}

/// `b` added into `a`, counter by counter: one measured in either is measured.
pub fn add(a: &mut agent::MeterCounters, b: &agent::MeterCounters) {
    a.cpu_usec = sum(a.cpu_usec, b.cpu_usec);
    a.cpu_throttled_usec = sum(a.cpu_throttled_usec, b.cpu_throttled_usec);
    a.io_read_bytes = sum(a.io_read_bytes, b.io_read_bytes);
    a.io_write_bytes = sum(a.io_write_bytes, b.io_write_bytes);
    a.net_rx_bytes = sum(a.net_rx_bytes, b.net_rx_bytes);
    a.net_tx_bytes = sum(a.net_tx_bytes, b.net_tx_bytes);
    a.memory_kib_seconds = sum(a.memory_kib_seconds, b.memory_kib_seconds);
    a.egress_bytes = sum(a.egress_bytes, b.egress_bytes);
    a.dedicated_core_seconds = sum(a.dedicated_core_seconds, b.dedicated_core_seconds);
    for d in &b.device_seconds {
        match a.device_seconds.iter_mut().find(|x| x.class == d.class) {
            Some(x) => x.seconds = x.seconds.saturating_add(d.seconds),
            None => a.device_seconds.push(d.clone()),
        }
    }
    for e in &b.endpoints {
        match a.endpoints.iter_mut().find(|x| x.endpoint == e.endpoint) {
            Some(x) => {
                x.received_bytes = x.received_bytes.saturating_add(e.received_bytes);
                x.received_packets = x.received_packets.saturating_add(e.received_packets);
                x.sent_bytes = x.sent_bytes.saturating_add(e.sent_bytes);
                x.sent_packets = x.sent_packets.saturating_add(e.sent_packets);
            }
            None => a.endpoints.push(e.clone()),
        }
    }
    a.endpoints.sort_by(|x, y| x.endpoint.cmp(&y.endpoint));
}

/// What `before` counted of the endpoints `now` no longer counts: an endpoint whose accounting
/// was turned off or that was renamed keeps what it carried, as a source that ended does.
fn dropped(before: &agent::MeterCounters, now: &agent::MeterCounters) -> agent::MeterCounters {
    let gone =
        before.endpoints.iter().filter(|e| !now.endpoints.iter().any(|n| n.endpoint == e.endpoint));
    agent::MeterCounters { endpoints: gone.cloned().collect(), ..Default::default() }
}

/// Whether any counter of `now` is below the same counter of `before`.
fn went_back(before: &agent::MeterCounters, now: &agent::MeterCounters) -> bool {
    let pairs = [
        (before.cpu_usec, now.cpu_usec),
        (before.cpu_throttled_usec, now.cpu_throttled_usec),
        (before.io_read_bytes, now.io_read_bytes),
        (before.io_write_bytes, now.io_write_bytes),
        (before.net_rx_bytes, now.net_rx_bytes),
        (before.net_tx_bytes, now.net_tx_bytes),
        (before.egress_bytes, now.egress_bytes),
    ];
    let endpoint = |e: &agent::EndpointCounters| {
        now.endpoints
            .iter()
            .find(|n| n.endpoint == e.endpoint)
            .is_some_and(|n| n.received_bytes < e.received_bytes || n.sent_bytes < e.sent_bytes)
    };
    pairs.iter().any(|(b, n)| matches!((b, n), (Some(b), Some(n)) if n < b))
        || before.endpoints.iter().any(endpoint)
}

/// The timestamp `t` is.
#[must_use]
pub fn at(t: SystemTime) -> buffa_types::google::protobuf::Timestamp {
    let since = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    buffa_types::google::protobuf::Timestamp {
        seconds: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(since.subsec_nanos()).unwrap_or(0),
        ..Default::default()
    }
}

/// When `t` was, by the timestamp it is.
#[must_use]
pub fn when(t: &buffa_types::google::protobuf::Timestamp) -> SystemTime {
    let seconds = u64::try_from(t.seconds).unwrap_or(0);
    let nanos = u32::try_from(t.nanos).unwrap_or(0);
    SystemTime::UNIX_EPOCH + Duration::new(seconds, nanos)
}

fn loss(from: SystemTime, to: SystemTime, cause: &str) -> agent::MeterLoss {
    agent::MeterLoss {
        from: at(from).into(),
        to: at(to).into(),
        cause: cause.to_owned(),
        ..Default::default()
    }
}

/// What one interval of a workload comes to when it closes.
#[derive(Debug, Clone, PartialEq)]
pub struct Closed {
    /// Its counters, cumulative.
    pub counters: agent::MeterCounters,
    /// The most memory read within the interval.
    pub peak: Option<u64>,
    /// What could not be measured since the last sample.
    pub losses: Vec<agent::MeterLoss>,
}

/// The meter's accounts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Book {
    read_at: Option<SystemTime>,
    workloads: BTreeMap<String, agent::WorkloadMeter>,
}

impl Book {
    /// The accounts its file kept.
    #[must_use]
    pub fn from_kept(kept: agent::MeterBook) -> Self {
        let read_at = kept.read_at.as_option().map(when);
        let workloads = kept.workloads.into_iter().map(|w| (w.workload.clone(), w)).collect();
        Self { read_at, workloads }
    }

    /// The accounts, for its file.
    #[must_use]
    pub fn to_kept(&self) -> agent::MeterBook {
        agent::MeterBook {
            read_at: self.read_at.map(at).into(),
            workloads: self.workloads.values().cloned().collect(),
            ..Default::default()
        }
    }

    /// When the meter last read every source.
    #[must_use]
    pub const fn read_at(&self) -> Option<SystemTime> {
        self.read_at
    }

    fn workload(&mut self, workload: &str) -> &mut agent::WorkloadMeter {
        self.workloads.entry(workload.to_owned()).or_insert_with(|| agent::WorkloadMeter {
            workload: workload.to_owned(),
            ..Default::default()
        })
    }

    /// The workloads it keeps accounts for.
    #[must_use]
    pub fn workloads(&self) -> Vec<String> {
        self.workloads.keys().cloned().collect()
    }

    /// Takes `reading` of `workload`'s `source`, read at `now`.
    pub fn read(
        &mut self,
        workload: &str,
        source: &str,
        reading: agent::MeterCounters,
        now: SystemTime,
    ) {
        let since = self.read_at().unwrap_or(now);
        let w = self.workload(workload);
        if let Some(before) = w.sources.get(source).cloned() {
            if went_back(&before, &reading) {
                add(w.base.get_or_insert_default(), &before);
                w.losses.push(loss(since, now, COUNTER_RESET));
            } else {
                add(w.base.get_or_insert_default(), &dropped(&before, &reading));
            }
        }
        w.sources.insert(source.to_owned(), reading);
    }

    /// Takes `reading` of `workload`'s `source` at `now`, or, where it went unread after it was
    /// read, ends it unread.
    pub fn read_or_end(
        &mut self,
        workload: &str,
        source: &str,
        reading: Option<agent::MeterCounters>,
        now: SystemTime,
    ) {
        match reading {
            Some(r) => self.read(workload, source, r, now),
            None if self.has_source(workload, source) => self.ended(workload, source, None, now),
            None => {}
        }
    }

    /// Integrates `object`'s memory in use, `current`, over the `elapsed` since the last reading,
    /// and keeps the interval's peak from it and from the kernel's `high_water` mark where that
    /// rose since the reading before.
    pub fn memory(
        &mut self,
        (workload, object): (&str, &str),
        (current, high_water): (Option<u64>, Option<u64>),
        elapsed: Duration,
    ) {
        let w = self.workload(workload);
        if let Some(bytes) = current {
            let kib_seconds = u128::from(bytes) * elapsed.as_millis() / (1024 * 1000);
            let kib_seconds = u64::try_from(kib_seconds).unwrap_or(u64::MAX);
            let base = w.base.get_or_insert_default();
            base.memory_kib_seconds = sum(base.memory_kib_seconds, Some(kib_seconds));
        }
        let mut peak = current;
        if let Some(mark) = high_water {
            if w.high_water.get(object).is_some_and(|before| mark > *before) {
                peak = peak.max(Some(mark));
            }
            w.high_water.insert(object.to_owned(), mark);
        }
        if peak.is_some() {
            w.interval_peak_bytes = w.interval_peak_bytes.max(peak);
        }
    }

    /// Integrates what `workload`'s placement holds on the node, `cores` dedicated cores and
    /// `devices` of each class, over the `elapsed` since the last reading: held, whatever ran on
    /// them (§2 of docs/15-observability.md). Nothing is counted for a placement holding none.
    pub fn held(
        &mut self,
        workload: &str,
        (cores, devices): (u64, &[(pb::DeviceClass, u64)]),
        elapsed: Duration,
    ) {
        let seconds = |n: u64| {
            let s = u128::from(n) * elapsed.as_millis() / 1000;
            u64::try_from(s).unwrap_or(u64::MAX)
        };
        let mut held = agent::MeterCounters::default();
        if cores > 0 {
            held.dedicated_core_seconds = Some(seconds(cores));
        }
        held.device_seconds = devices
            .iter()
            .filter(|(_, n)| *n > 0)
            .map(|(class, n)| pb::DeviceSeconds {
                class: (*class).into(),
                seconds: seconds(*n),
                ..Default::default()
            })
            .collect();
        if held != agent::MeterCounters::default() {
            add(self.workload(workload).base.get_or_insert_default(), &held);
        }
    }

    /// `workload`'s `source` ended, its last reading `last`, or none where it could not be read
    /// at its end: then its reading before is all of it the meter has, and since then is lost.
    pub fn ended(
        &mut self,
        workload: &str,
        source: &str,
        last: Option<agent::MeterCounters>,
        now: SystemTime,
    ) {
        let since = self.read_at().unwrap_or(now);
        let w = self.workload(workload);
        let before = w.sources.remove(source);
        w.high_water.remove(source);
        let last = match (last, before) {
            (Some(last), Some(before)) if went_back(&before, &last) => {
                w.losses.push(loss(since, now, COUNTER_RESET));
                add(w.base.get_or_insert_default(), &before);
                Some(last)
            }
            (Some(last), _) => Some(last),
            (None, before) => {
                w.losses.push(loss(since, now, ENDED_UNREAD));
                before
            }
        };
        if let Some(last) = last {
            add(w.base.get_or_insert_default(), &last);
        }
    }

    /// Every source's last reading joins its base: the machine restarted at `boot`, after the
    /// meter last read them, and nothing they used since can be read.
    pub fn rebooted(&mut self, boot: SystemTime) {
        let since = self.read_at().unwrap_or(boot);
        for w in self.workloads.values_mut() {
            let sources = std::mem::take(&mut w.sources);
            for last in sources.values() {
                add(w.base.get_or_insert_default(), last);
            }
            w.high_water.clear();
            w.losses.push(loss(since, boot, NODE_REBOOTED));
        }
    }

    /// The meter read every source at `now`.
    pub const fn read_all(&mut self, now: SystemTime) {
        self.read_at = Some(now);
    }

    /// What `workload`'s interval comes to now, changing nothing: its counters, its peak, and
    /// its losses.
    #[must_use]
    pub fn pending(&self, workload: &str) -> Closed {
        let Some(w) = self.workloads.get(workload) else {
            return Closed {
                counters: agent::MeterCounters::default(),
                peak: None,
                losses: vec![],
            };
        };
        let mut counters = w.base.as_option().cloned().unwrap_or_default();
        for reading in w.sources.values() {
            add(&mut counters, reading);
        }
        Closed { counters, peak: w.interval_peak_bytes, losses: w.losses.clone() }
    }

    /// Closes `workload`'s interval, once its sample is journaled: the peak and the losses begin
    /// again. What it came to.
    pub fn close(&mut self, workload: &str) -> Closed {
        let closed = self.pending(workload);
        let w = self.workload(workload);
        w.interval_peak_bytes = None;
        w.losses.clear();
        closed
    }

    /// `workload` could not be measured from `from` to `to`, for `cause`.
    pub fn lost(&mut self, workload: &str, (from, to): (SystemTime, SystemTime), cause: &str) {
        self.workload(workload).losses.push(loss(from, to, cause));
    }

    /// Whether `workload` has a running `source`.
    #[must_use]
    pub fn has_source(&self, workload: &str, source: &str) -> bool {
        self.workloads.get(workload).is_some_and(|w| w.sources.contains_key(source))
    }

    /// Forgets `workload`, once it runs here no more and has no source still running.
    pub fn forget_if_idle(&mut self, workload: &str) {
        if !self.running(workload) {
            self.workloads.remove(workload);
        }
    }

    /// Each of `workloads` could not be measured over `span`, for `cause`.
    pub fn lost_all(&mut self, workloads: &[String], span: (SystemTime, SystemTime), cause: &str) {
        for w in workloads {
            self.lost(w, span, cause);
        }
    }

    /// Whether `workload` has a source still running.
    #[must_use]
    pub fn running(&self, workload: &str) -> bool {
        self.workloads.get(workload).is_some_and(|w| !w.sources.is_empty())
    }
}

#[cfg(test)]
mod tests;
