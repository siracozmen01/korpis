// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node journal (§4.10 of docs/02-architecture.md): every record a node originates, numbered
//! by a per-node sequence and ingested exactly once, in order. The node's journal is the buffer:
//! anything not acknowledged is sent again, so the control plane keeps nothing beyond the next
//! record it expects.

use crate::audit::Outcome;
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
    /// Records lost because the journal filled: a visible loss.
    Gap {
        /// The first lost.
        first: u64,
        /// The last lost.
        last: u64,
        /// Why.
        cause: String,
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

/// The records to ingest after `cursor`: in sequence order, one past the cursor and onward
/// without a gap. Records at or below the cursor were ingested and are duplicates; records past
/// a gap wait until the node sends what is missing, which it does because nothing past the
/// cursor was acknowledged.
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
        if r.seq > next {
            break;
        }
        next = next.saturating_add(1);
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
        assert!(valid_action("workload.start") && !valid_action("Workload Start"));
    }
}
