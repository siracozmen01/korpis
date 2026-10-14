// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node journal (§4.10 of docs/02-architecture.md): every record a node originates, numbered
//! by a per-node sequence and ingested exactly once, in order. The node's journal is the buffer:
//! anything not acknowledged is sent again, so the control plane keeps nothing beyond the next
//! record it expects.

use crate::audit::Outcome;
use crate::workload::stream::{Segment, SegmentEvent, StreamKind};
use serde_json::{Map, Value};
use std::time::SystemTime;
use uuid::Uuid;

/// The longest action name a node may report.
const MAX_ACTION: usize = 64;

/// An effect a node reports: something that happened on it.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeEffect {
    /// The workload it concerns.
    pub workload: Option<Uuid>,
    /// The plan it executes a step of.
    pub plan: Option<Uuid>,
    /// The step.
    pub step: Option<u32>,
    /// What happened, e.g. `workload.start`.
    pub action: String,
    /// How it turned out.
    pub outcome: Outcome,
    /// For failures: what went wrong.
    pub error: Option<Map<String, Value>>,
    /// What it left behind.
    pub after: Option<Map<String, Value>>,
}

/// What a record holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    /// An effect.
    Effect(NodeEffect),
    /// A metering sample (§2 of docs/15-observability.md).
    Metering,
    /// A break-glass session opened on the machine (§10 of docs/02-architecture.md).
    BreakGlass {
        /// The operator, as the machine knows them.
        operator: String,
        /// Why, as they said.
        reason: String,
        /// The session.
        session: String,
    },
    /// The node stopped acting under a lease epoch.
    LeaseReleased {
        /// The workload.
        workload: Uuid,
        /// The epoch.
        epoch: u64,
    },
    /// Records the node no longer has: lost because its journal filled, discarded before a
    /// control plane restored from an older backup ingested them, or unreadable on its disk. A
    /// gap stands for the sequence numbers it names, and its own is the last of them.
    Gap {
        /// The first lost.
        first: u64,
        /// The last lost.
        last: u64,
        /// Why.
        cause: String,
    },
    /// A segment of a workload's stream was sealed, offloaded, or pruned on the node (§4 of
    /// docs/14-streams.md).
    Segment {
        /// The workload.
        workload: Uuid,
        /// Which of its streams.
        kind: StreamKind,
        /// What happened.
        event: SegmentEvent,
        /// The segment as it stands after it; its node is the journal's.
        segment: Segment,
    },
    /// A record that could not be read as one: kept as a rejection, so one bad record never
    /// blocks a journal.
    Malformed {
        /// What was wrong with it.
        reason: String,
    },
}

/// One record.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// Monotonic per node, never reused.
    pub seq: u64,
    /// The node's clock, informational.
    pub occurred_at: Option<SystemTime>,
    /// What it holds.
    pub body: Body,
    /// Coalesced identical records: how many this one stands for.
    pub count: u64,
}

impl Record {
    /// The first sequence number the record stands for: a gap's first, any other record's own.
    #[must_use]
    pub fn first(&self) -> u64 {
        match &self.body {
            Body::Gap { first, .. } if *first <= self.seq => *first,
            _ => self.seq,
        }
    }
}

/// The records to ingest after `cursor`: in sequence order, each standing for the number one
/// past the cursor, onward without a gap. Records at or below the cursor were ingested and are
/// duplicates; records past a hole wait until the node sends what is missing, which it does
/// because nothing past the cursor was acknowledged. A gap record fills a hole by standing for
/// every number it names.
#[must_use]
pub fn admit(cursor: u64, mut records: Vec<Record>) -> Vec<Record> {
    records.sort_by_key(|r| r.seq);
    records.dedup_by_key(|r| r.seq);
    let mut next = cursor.saturating_add(1);
    let mut out = Vec::new();
    for r in records {
        if r.seq < next {
            continue;
        }
        if r.first() > next {
            break;
        }
        next = r.seq.saturating_add(1);
        out.push(r);
    }
    out
}

/// Whether `action` is a name a node may report: lowercase letters, digits, dots and
/// underscores, at most 64 bytes.
#[must_use]
pub fn valid_action(action: &str) -> bool {
    !action.is_empty()
        && action.len() <= MAX_ACTION
        && action
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seq: u64) -> Record {
        Record { seq, occurred_at: None, body: Body::Metering, count: 1 }
    }

    #[test]
    fn records_are_admitted_once_in_order_and_not_past_a_gap() {
        let seqs = |rs: Vec<Record>| rs.into_iter().map(|r| r.seq).collect::<Vec<_>>();
        let batch = vec![record(5), record(3), record(4), record(4), record(2), record(7)];
        assert_eq!(seqs(admit(2, batch)), vec![3, 4, 5], "2 is a duplicate, 7 waits for 6");
        assert!(admit(9, vec![record(3)]).is_empty());
        let gap = |seq, first| Record {
            body: Body::Gap { first, last: seq, cause: "journal exhausted".into() },
            ..record(seq)
        };
        let filled = admit(2, vec![record(9), gap(8, 3), record(10)]);
        assert_eq!(seqs(filled), vec![8, 9, 10], "the gap stands for 3 through 8");
        assert!(admit(2, vec![gap(8, 5)]).is_empty(), "3 and 4 are still missing");
        assert_eq!(seqs(admit(4, vec![gap(8, 3)])), vec![8], "a gap may overlap the cursor");
        assert!(valid_action("workload.start") && !valid_action("Workload Start"));
    }
}
