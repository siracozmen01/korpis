// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's streams as the control plane knows them (§3.6 of docs/01-model.md): the index of
//! their segments, never their bytes (§4 of docs/14-streams.md). The node that writes a stream
//! seals, offloads, and prunes its segments and journals each; the index is kept from those
//! records, so an offloaded segment stays findable when its node is gone, and what is held only
//! on a node is known by how much.

use std::time::SystemTime;
use uuid::Uuid;

/// What a stream carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    /// The workload's console: its output, and the input typed into it.
    Console,
    /// A log file it writes.
    Log,
    /// Events.
    Event,
    /// Metrics.
    Metric,
}

impl StreamKind {
    /// Its stored name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Log => "log",
            Self::Event => "event",
            Self::Metric => "metric",
        }
    }

    /// The kind a stored name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Console, Self::Log, Self::Event, Self::Metric].into_iter().find(|k| k.as_str() == s)
    }
}

/// A contiguous run of a stream's records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The offset of its first record.
    pub first_offset: u64,
    /// One past its last.
    pub end_offset: u64,
    /// The node-clock times of its first and last records; informational.
    pub first_at: SystemTime,
    /// The last.
    pub last_at: SystemTime,
    /// Its size.
    pub bytes: u64,
    /// `sha256:` and its digest once sealed.
    pub digest: String,
    /// The node holding it, while one does.
    pub node: Option<Uuid>,
    /// Held by the offload target.
    pub offloaded: bool,
}

/// What happened to a segment on its node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentEvent {
    /// Sealed: its digest is final.
    Sealed,
    /// Held by the offload target as well.
    Offloaded,
    /// Removed from the node to its retention.
    Pruned,
}

impl SegmentEvent {
    /// The action of the effect that records it.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Sealed => "stream.segment.seal",
            Self::Offloaded => "stream.segment.offload",
            Self::Pruned => "stream.segment.prune",
        }
    }
}

/// A workload's stream, with its index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// Immutable.
    pub id: Uuid,
    /// The workload it belongs to.
    pub workload: Uuid,
    /// What it carries.
    pub kind: StreamKind,
    /// Its segments, oldest first.
    pub segments: Vec<Segment>,
}

impl Stream {
    /// Bytes held only on a node, not offloaded: what the node's loss would lose (§4 of
    /// docs/14-streams.md).
    #[must_use]
    pub fn node_only_bytes(&self) -> u64 {
        let only = self.segments.iter().filter(|s| s.node.is_some() && !s.offloaded);
        only.fold(0u64, |a, s| a.saturating_add(s.bytes))
    }

    /// Offsets no longer held anywhere: the ranges between and before the segments it holds,
    /// down to the stream's first offset.
    #[must_use]
    pub fn lost(&self) -> Vec<(u64, u64)> {
        let mut lost = Vec::new();
        let mut next = 0;
        for s in self.segments.iter().filter(|s| s.node.is_some() || s.offloaded) {
            if s.first_offset > next {
                lost.push((next, s.first_offset));
            }
            next = next.max(s.end_offset);
        }
        lost
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(first: u64, end: u64, node: bool, offloaded: bool) -> Segment {
        Segment {
            first_offset: first,
            end_offset: end,
            first_at: SystemTime::UNIX_EPOCH,
            last_at: SystemTime::UNIX_EPOCH,
            bytes: 100,
            digest: String::new(),
            node: node.then(Uuid::now_v7),
            offloaded,
        }
    }

    #[test]
    fn the_index_says_what_a_node_holds_alone_and_what_is_gone() {
        let stream = Stream {
            id: Uuid::now_v7(),
            workload: Uuid::now_v7(),
            kind: StreamKind::Console,
            segments: vec![
                segment(0, 10, false, false),
                segment(10, 20, true, true),
                segment(20, 30, true, false),
                segment(40, 50, true, false),
            ],
        };
        assert_eq!(stream.node_only_bytes(), 200);
        assert_eq!(stream.lost(), [(0, 10), (30, 40)]);
    }
}
