// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metering (§2 of docs/15-observability.md, "Measuring it, as designed"): what a node's sample
//! says, and what the control plane makes of it. A sample carries a workload's counters since it
//! first ran on its node, so what an interval used is the difference of two samples of the same
//! workload on the same node, and samples lost between them leave one record spanning their
//! intervals: the total measured, its division among them not.

use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// A span samples were lost over, between the last one received and the next.
pub const SAMPLES_LOST: &str = "samples lost";
/// A node's counters began again: its meter's accounts were lost, so what ran between its last
/// sample and the new count is unknown.
pub const COUNTER_RESET: &str = "counter reset";

/// Counters, cumulative in a sample and the differences in a record. Each is absent where
/// nothing measured it, never zero for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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
}

impl Counters {
    fn pairs(&self) -> [Option<u64>; 7] {
        [
            self.cpu_usec,
            self.cpu_throttled_usec,
            self.io_read_bytes,
            self.io_write_bytes,
            self.net_rx_bytes,
            self.net_tx_bytes,
            self.memory_kib_seconds,
        ]
    }

    fn from_pairs(p: [Option<u64>; 7]) -> Self {
        let [cpu, throttled, read, written, rx, tx, memory] = p;
        Self {
            cpu_usec: cpu,
            cpu_throttled_usec: throttled,
            io_read_bytes: read,
            io_write_bytes: written,
            net_rx_bytes: rx,
            net_tx_bytes: tx,
            memory_kib_seconds: memory,
        }
    }

    /// Whether any counter is below the same counter of `before`.
    #[must_use]
    pub fn below(&self, before: &Self) -> bool {
        self.pairs()
            .iter()
            .zip(before.pairs())
            .any(|(n, b)| matches!((n, b), (Some(n), Some(b)) if *n < b))
    }

    /// What was used since `before`, counter by counter. A counter `before` did not measure is
    /// counted from zero, since a node's counter begins at zero when it is first measured.
    #[must_use]
    pub fn since(&self, before: &Self) -> Self {
        let mut out = [None; 7];
        for (o, (n, b)) in out.iter_mut().zip(self.pairs().into_iter().zip(before.pairs())) {
            *o = n.map(|n| n.saturating_sub(b.unwrap_or(0)));
        }
        Self::from_pairs(out)
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
    /// What the node could not measure since its previous sample.
    pub losses: Vec<Loss>,
}

impl Sample {
    /// The interval's end.
    #[must_use]
    pub fn end(&self) -> SystemTime {
        self.interval_start + self.interval
    }
}

/// The last sample of a workload on a node the control plane took: where the next is measured
/// from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// The record it replaces.
    pub supersedes: Option<Uuid>,
    /// What was used.
    pub used: Counters,
    /// The most memory used within it.
    pub memory_peak_bytes: Option<u64>,
    /// The bytes its volumes held at its end.
    pub disk_bytes: Option<u64>,
    /// What the node could not measure within it.
    pub losses: Vec<Loss>,
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
            supersedes: None,
            used: m.used,
            memory_peak_bytes: sample.memory_peak_bytes,
            disk_bytes: sample.disk_bytes,
            losses: sample.losses.clone(),
        }
    }
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
        let used = sample.counters;
        return Ok(Measured { from: sample.interval_start, to, used, cause: None });
    };
    if to <= before.end {
        return Err(Unmeasured::Stale);
    }
    if sample.counters.below(&before.counters) {
        // What ran between the reading before and the count beginning again is unknown.
        let used = sample.counters;
        return Ok(Measured { from: sample.interval_start, to, used, cause: Some(COUNTER_RESET) });
    }
    let used = sample.counters.since(&before.counters);
    let cause = (before.end < sample.interval_start).then_some(SAMPLES_LOST);
    Ok(Measured { from: before.end.min(sample.interval_start), to, used, cause })
}

#[cfg(test)]
mod tests;
