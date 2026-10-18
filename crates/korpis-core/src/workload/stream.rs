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

    /// The volume `workload`'s stream of this kind is written to on the node that runs it. It is
    /// named for the workload, so no spec can point a writer at another workload's stream, and
    /// its `_` is a character no declared volume's name may hold, so it never meets one.
    #[must_use]
    pub fn volume(self, workload: &str) -> String {
        format!("{workload}.{}", self.volume_name())
    }

    /// The node's volume holding a stream of this kind of `run` of `workload`: a run's console
    /// is a stream of its own (§4 of docs/14-streams.md).
    #[must_use]
    pub fn run_volume(self, workload: &str, run: &str) -> String {
        format!("{workload}.{}.{run}", self.volume_name())
    }

    /// The name the workload's plans give the volume holding a stream of this kind: what a
    /// deletion names to delete the stream (§4 of docs/14-streams.md).
    #[must_use]
    pub fn volume_name(self) -> String {
        format!("stream_{}", self.as_str())
    }

    /// The kind of stream the volume named `name` holds, if it holds one.
    #[must_use]
    pub fn of_volume(name: &str) -> Option<Self> {
        Self::parse(name.strip_prefix("stream_")?)
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
    /// The repository holding it once offloaded (§4 of docs/14-streams.md).
    pub repository: Option<Uuid>,
}

/// The name of the object an offloaded segment is kept as, beneath its workload's stream prefix
/// (§4 of docs/14-streams.md): its first offset and the hexadecimal digits of its digest, so the
/// same segment offloaded twice is one object and a listing reads in offset order, beneath
/// `runs/` and its run's identifier for a run's stream, whose offsets start from zero again.
/// `None` for a digest that is not `sha256:` and 64 lowercase hexadecimal digits.
/// `korpis-backup` writes by the same rule, which it cannot take from here, and a test holds the
/// two to it.
#[must_use]
pub fn segment_object(run: Option<Uuid>, first_offset: u64, digest: &str) -> Option<String> {
    let hex = digest.strip_prefix("sha256:")?;
    let plain = hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let runs =
        run.map(|r| format!("runs/{}/", crate::id::Id::from_uuid(crate::id::IdKind::Run, r)));
    plain.then(|| format!("segments/{}{first_offset:020}-{hex}", runs.unwrap_or_default()))
}

impl Segment {
    /// Whether it is held by the repository that takes its stream.
    #[must_use]
    pub const fn offloaded(&self) -> bool {
        self.repository.is_some()
    }
}

/// Which stream: a workload's of a kind, and for a task or scheduled workload's console one
/// run's, each run's its own (§4 of docs/14-streams.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamKey {
    /// The workload.
    pub workload: Uuid,
    /// What it carries.
    pub kind: StreamKind,
    /// The run, for a run's console.
    pub run: Option<Uuid>,
}

impl StreamKey {
    /// `workload`'s stream of `kind`, of no run.
    #[must_use]
    pub const fn of(workload: Uuid, kind: StreamKind) -> Self {
        Self { workload, kind, run: None }
    }
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
    /// The run whose console it is, for a task or scheduled workload, each run's its own (§4 of
    /// docs/14-streams.md).
    pub run: Option<Uuid>,
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
        let only = self.segments.iter().filter(|s| s.node.is_some() && !s.offloaded());
        only.fold(0u64, |a, s| a.saturating_add(s.bytes))
    }

    /// Offsets no longer held anywhere: the ranges before, between, and after the segments it
    /// holds, from the stream's first offset to the last one the index has heard of. A stream
    /// whose volume was deleted, with nothing offloaded, is lost whole.
    #[must_use]
    pub fn lost(&self) -> Vec<(u64, u64)> {
        let mut lost = Vec::new();
        let mut next = 0;
        for s in self.segments.iter().filter(|s| s.node.is_some() || s.offloaded()) {
            if s.first_offset > next {
                lost.push((next, s.first_offset));
            }
            next = next.max(s.end_offset);
        }
        let heard = self.segments.iter().map(|s| s.end_offset).max().unwrap_or(0);
        if heard > next {
            lost.push((next, heard));
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
            repository: offloaded.then(Uuid::now_v7),
        }
    }

    #[test]
    fn a_run_s_segments_are_named_beneath_its_run_and_a_workload_s_are_not() {
        let digest = format!("sha256:{}", "0a".repeat(32));
        let run = Uuid::from_u128(7);
        let named = segment_object(Some(run), 3, &digest).unwrap();
        let id = crate::id::Id::from_uuid(crate::id::IdKind::Run, run);
        assert_eq!(named, format!("segments/runs/{id}/00000000000000000003-{}", "0a".repeat(32)));
        assert!(segment_object(None, 3, &digest).unwrap().starts_with("segments/0000"));
        assert!(segment_object(Some(run), 3, "sha256:../x").is_none());
    }

    #[test]
    fn the_index_says_what_a_node_holds_alone_and_what_is_gone() {
        let stream = Stream {
            run: None,
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

    #[test]
    fn what_is_gone_after_the_last_segment_held_is_lost_too() {
        let mut stream = Stream {
            run: None,
            id: Uuid::now_v7(),
            workload: Uuid::now_v7(),
            kind: StreamKind::Console,
            segments: vec![segment(0, 10, true, false), segment(10, 20, false, false)],
        };
        assert_eq!(stream.lost(), [(10, 20)]);
        stream.segments[0].node = None;
        assert_eq!(stream.lost(), [(0, 20)], "nothing held: lost whole");
    }
}
