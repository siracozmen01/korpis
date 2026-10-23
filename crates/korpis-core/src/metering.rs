// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metering (§2 of docs/15-observability.md, "Measuring it, as designed"): what a node's sample
//! says, and what the control plane makes of it. A sample carries a workload's counters since it
//! first ran on its node, so what an interval used is the difference of two samples of the same
//! workload on the same node, and samples lost between them leave one record spanning their
//! intervals: the total measured, its division among them not.

use std::time::{Duration, SystemTime};
use uuid::Uuid;

mod endpoints;
mod period;

pub use endpoints::{EndpointUse, Endpoints};
pub use period::{Period, PeriodState};

/// A span samples were lost over, between the last one received and the next.
pub const SAMPLES_LOST: &str = "samples lost";
/// An interval of a workload a node holds that no record covered two intervals after it ended.
pub const NODE_UNREACHABLE: &str = "node unreachable";
/// A day of metering, the period records are closed by.
pub const DAY: Duration = Duration::from_secs(86_400);

/// The start of the UTC day `t` is in.
#[must_use]
pub fn day_of(t: SystemTime) -> SystemTime {
    let since = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
    SystemTime::UNIX_EPOCH + Duration::from_secs(since / DAY.as_secs() * DAY.as_secs())
}

/// The start of the interval of length `interval` that `t` is in.
#[must_use]
pub fn interval_of(t: SystemTime, interval: Duration) -> SystemTime {
    let since = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs();
    let step = interval.as_secs().max(1);
    SystemTime::UNIX_EPOCH + Duration::from_secs(since / step * step)
}

/// A node's counters began again: its meter's accounts were lost, so what ran between its last
/// sample and the new count is unknown.
pub const COUNTER_RESET: &str = "counter reset";

/// Counters, cumulative in a sample and the differences in a record. Each is absent where
/// nothing measured it, never zero for that.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Counters {
    /// CPU time, in microseconds.
    pub cpu_usec: Option<u64>,
    /// Time throttled, in microseconds.
    pub cpu_throttled_usec: Option<u64>,
    /// Bytes read from block devices.
    pub io_read_bytes: Option<u64>,
    /// Bytes written to block devices.
    pub io_write_bytes: Option<u64>,
    /// Bytes the workload's interface received.
    pub net_rx_bytes: Option<u64>,
    /// Bytes it sent.
    pub net_tx_bytes: Option<u64>,
    /// Memory in use integrated over time, in KiB-seconds.
    pub memory_kib_seconds: Option<u64>,
    /// Bytes that left the workload's link for anything but another workload's on its node.
    pub egress_bytes: Option<u64>,
    /// What each endpoint whose accounting is on carried, by name.
    pub endpoints: Endpoints,
}

impl Counters {
    fn pairs(&self) -> [Option<u64>; 8] {
        [
            self.cpu_usec,
            self.cpu_throttled_usec,
            self.io_read_bytes,
            self.io_write_bytes,
            self.net_rx_bytes,
            self.net_tx_bytes,
            self.memory_kib_seconds,
            self.egress_bytes,
        ]
    }

    fn from_pairs(p: [Option<u64>; 8], endpoints: Endpoints) -> Self {
        let [cpu, throttled, read, written, rx, tx, memory, egress] = p;
        Self {
            cpu_usec: cpu,
            cpu_throttled_usec: throttled,
            io_read_bytes: read,
            io_write_bytes: written,
            net_rx_bytes: rx,
            net_tx_bytes: tx,
            memory_kib_seconds: memory,
            egress_bytes: egress,
            endpoints,
        }
    }

    /// Whether any counter is below the same counter of `before`.
    #[must_use]
    pub fn below(&self, before: &Self) -> bool {
        self.pairs()
            .iter()
            .zip(before.pairs())
            .any(|(n, b)| matches!((n, b), (Some(n), Some(b)) if *n < b))
            || endpoints::below(&self.endpoints, &before.endpoints)
    }

    /// What was used since `before`, counter by counter. A counter `before` did not measure is
    /// counted from zero, since a node's counter begins at zero when it is first measured.
    #[must_use]
    pub fn since(&self, before: &Self) -> Self {
        let mut out = [None; 8];
        for (o, (n, b)) in out.iter_mut().zip(self.pairs().into_iter().zip(before.pairs())) {
            *o = n.map(|n| n.saturating_sub(b.unwrap_or(0)));
        }
        Self::from_pairs(out, endpoints::since(&self.endpoints, &before.endpoints))
    }
}

/// A span the node could not measure, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loss {
    /// From.
    pub from: SystemTime,
    /// To.
    pub to: SystemTime,
    /// Why, in the node's words.
    pub cause: String,
}

/// One workload's meter at the end of one interval, as its node journaled it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// The workload.
    pub workload: Uuid,
    /// The interval's start, by the node's clock.
    pub interval_start: SystemTime,
    /// Its length.
    pub interval: Duration,
    /// The lease epoch it ran under there.
    pub lease_epoch: u64,
    /// Its counters since it first ran on the node.
    pub counters: Counters,
    /// The most memory it used within the interval.
    pub memory_peak_bytes: Option<u64>,
    /// The bytes its volumes held at the sample.
    pub disk_bytes: Option<u64>,
    /// Those bytes are what block devices' images have allocated, not what their guests hold.
    pub disk_allocated: bool,
    /// What the node could not measure since its previous sample.
    pub losses: Vec<Loss>,
}

impl Sample {
    /// The interval's end; its start where the two would not add, which [`implausible`] refuses
    /// before anything measures it.
    #[must_use]
    pub fn end(&self) -> SystemTime {
        self.interval_start.checked_add(self.interval).unwrap_or(self.interval_start)
    }
}

/// The longest one sample may cover: a node's journal keeps a week of records past what was
/// acknowledged, so a node that sampled after a longer silence still has its samples to send,
/// and a sample is never more than that.
pub const LONGEST_SAMPLE: Duration = Duration::from_secs(31 * 86_400);
/// How far before the control plane's clock a sample or a loss may lie.
pub const OLDEST: Duration = Duration::from_secs(400 * 86_400);
/// How far ahead of it: a node's clock that runs ahead by more is not believed.
pub const AHEAD: Duration = Duration::from_secs(3600);

/// Why `sample` cannot be believed at `now`, if it cannot: a node's word is checked before it
/// is stored, so one record no store could hold rejects that record rather than stopping its
/// node's journal behind it (§4.10 of docs/02-architecture.md).
#[must_use]
pub fn implausible(sample: &Sample, now: SystemTime) -> Option<&'static str> {
    let oldest = now.checked_sub(OLDEST).unwrap_or(SystemTime::UNIX_EPOCH);
    let latest = now.checked_add(AHEAD).unwrap_or(now);
    let within = |t: SystemTime| oldest <= t && t <= latest;
    if sample.interval.is_zero() || sample.interval > LONGEST_SAMPLE {
        return Some("a sample's interval is longer than any one sample covers");
    }
    let end = sample.interval_start.checked_add(sample.interval);
    if !within(sample.interval_start) || !end.is_some_and(within) {
        return Some("a sample's interval is not near the control plane's clock");
    }
    if sample.losses.iter().any(|l| !within(l.from) || !within(l.to) || l.to < l.from) {
        return Some("a loss is not near the control plane's clock");
    }
    let fits = |n: Option<u64>| n.is_none_or(|n| i64::try_from(n).is_ok());
    let counted =
        sample.counters.pairs().into_iter().chain([sample.memory_peak_bytes, sample.disk_bytes]);
    let endpoints = sample.counters.endpoints.iter();
    let endpoint_fits = endpoints.flat_map(|(_, e)| e.fields()).all(|n| i64::try_from(n).is_ok());
    if !counted.into_iter().all(fits)
        || !endpoint_fits
        || i64::try_from(sample.lease_epoch).is_err()
    {
        return Some("a counter is beyond what is stored");
    }
    None
}

/// The last sample of a workload on a node the control plane took: where the next is measured
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The end of its interval.
    pub end: SystemTime,
    /// Its counters.
    pub counters: Counters,
}

/// What a sample comes to, measured from the reading before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measured {
    /// The start of what it covers: its interval's, or the end of the reading before where
    /// samples between were lost.
    pub from: SystemTime,
    /// The end.
    pub to: SystemTime,
    /// What was used over it.
    pub used: Counters,
    /// Why it spans more than its interval, or begins a count again.
    pub cause: Option<&'static str>,
}

/// What a record says of its interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordKind {
    /// What was measured.
    Measured,
    /// That nothing was heard, and why: superseded if a measurement arrives.
    Gap,
    /// A record that arrived after its period closed, written into the open one.
    Adjustment,
}

impl RecordKind {
    /// Its stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::Gap => "gap",
            Self::Adjustment => "adjustment",
        }
    }

    /// The kind a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Measured, Self::Gap, Self::Adjustment].into_iter().find(|k| k.as_str() == s)
    }
}

/// What one workload used over one interval, or one span of lost samples. Never changed: a
/// later record that covers it names it as the one it supersedes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// Its identifier.
    pub id: Uuid,
    /// The workload.
    pub workload: Uuid,
    /// The node that measured it; none for what the control plane writes itself.
    pub node: Option<Uuid>,
    /// The lease epoch it ran under.
    pub lease_epoch: u64,
    /// From.
    pub from: SystemTime,
    /// To.
    pub to: SystemTime,
    /// What it says.
    pub kind: RecordKind,
    /// Why it spans more than an interval, begins a count again, or is a gap.
    pub cause: Option<String>,
    /// The records it replaces: the gaps it covers.
    pub supersedes: Vec<Uuid>,
    /// What was used.
    pub used: Counters,
    /// The most memory used within it.
    pub memory_peak_bytes: Option<u64>,
    /// The bytes its volumes held at its end.
    pub disk_bytes: Option<u64>,
    /// Those bytes are what block devices' images have allocated (§3.1 of docs/06-storage.md).
    pub disk_allocated: bool,
    /// What the node could not measure within it.
    pub losses: Vec<Loss>,
    /// When the control plane recorded it, once it has: an adjustment's date.
    pub recorded_at: Option<SystemTime>,
}

impl Record {
    /// The record `sample`, measured as `m`, makes, taken on `node`.
    #[must_use]
    pub fn measured(sample: &Sample, node: Uuid, m: &Measured) -> Self {
        Self {
            id: Uuid::now_v7(),
            workload: sample.workload,
            node: Some(node),
            lease_epoch: sample.lease_epoch,
            from: m.from,
            to: m.to,
            kind: RecordKind::Measured,
            cause: m.cause.map(str::to_owned),
            supersedes: Vec::new(),
            used: m.used.clone(),
            memory_peak_bytes: sample.memory_peak_bytes,
            disk_bytes: sample.disk_bytes,
            disk_allocated: sample.disk_allocated,
            losses: sample.losses.clone(),
            recorded_at: None,
        }
    }
}

impl Record {
    /// A gap: nothing of `workload` was heard from `node` over `from` to `to`.
    #[must_use]
    pub fn gap(workload: Uuid, node: Uuid, (from, to): (SystemTime, SystemTime)) -> Self {
        Self {
            id: Uuid::now_v7(),
            workload,
            node: Some(node),
            lease_epoch: 0,
            from,
            to,
            kind: RecordKind::Gap,
            cause: Some(NODE_UNREACHABLE.to_owned()),
            supersedes: Vec::new(),
            used: Counters::default(),
            memory_peak_bytes: None,
            disk_bytes: None,
            disk_allocated: false,
            losses: Vec::new(),
            recorded_at: None,
        }
    }
}

/// The intervals of length `interval` from `from` to `to` not yet covered: each a gap's span.
#[must_use]
pub fn gaps(from: SystemTime, to: SystemTime, interval: Duration) -> Vec<(SystemTime, SystemTime)> {
    let mut out = Vec::new();
    let mut at = interval_of(from, interval);
    while at + interval <= to {
        out.push((at.max(from), at + interval));
        at += interval;
    }
    out
}

/// Why a sample measures nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Unmeasured {
    /// It ends no later than the reading before it.
    #[error("the sample ends no later than the one before it")]
    Stale,
}

/// What `sample` measured since `before`, the last reading of its workload on its node.
pub fn measure(sample: &Sample, before: Option<&Reading>) -> Result<Measured, Unmeasured> {
    let to = sample.end();
    let Some(before) = before else {
        // The first sample of a workload on a node counts from zero, at the start of its interval.
        let used = sample.counters.clone();
        return Ok(Measured { from: sample.interval_start, to, used, cause: None });
    };
    if to <= before.end {
        return Err(Unmeasured::Stale);
    }
    if sample.counters.below(&before.counters) {
        // What ran between the reading before and the count beginning again is unknown.
        let used = sample.counters.clone();
        return Ok(Measured { from: sample.interval_start, to, used, cause: Some(COUNTER_RESET) });
    }
    let used = sample.counters.since(&before.counters);
    let cause = (before.end < sample.interval_start).then_some(SAMPLES_LOST);
    Ok(Measured { from: before.end.min(sample.interval_start), to, used, cause })
}

#[cfg(test)]
mod tests;
