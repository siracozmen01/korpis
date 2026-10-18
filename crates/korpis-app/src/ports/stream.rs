// SPDX-License-Identifier: AGPL-3.0-or-later

//! Streams' segment index (§4 of docs/14-streams.md): kept from what nodes journal, read to say
//! where a stream's records are and what is held only on a node.

use super::StoreResult;
use korpis_core::workload::stream::{Segment, SegmentEvent, Stream, StreamKey, StreamKind};
use std::future::Future;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// An offloaded segment past its retention, as the pass that deletes it reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expired {
    /// Its stream.
    pub stream: StreamKey,
    /// Its first offset, which with its digest names its object.
    pub first_offset: u64,
    /// Its digest.
    pub digest: String,
    /// The repository holding it.
    pub repository: Uuid,
}

/// A segment as a node reported it, and when.
#[derive(Debug, Clone, Copy)]
pub struct Reported<'a> {
    /// What happened to it.
    pub event: SegmentEvent,
    /// It, as it stands after.
    pub segment: &'a Segment,
    /// The node that reported it.
    pub node: Uuid,
    /// When the control plane heard it, by its own clock.
    pub at: SystemTime,
}

/// Reading streams.
pub trait StreamRead {
    /// The stream `key` names, with its index; none before its first segment.
    fn stream(&self, key: StreamKey) -> impl Future<Output = StoreResult<Option<Stream>>> + Send;

    /// Offloaded segments, the oldest offloaded first, at most `limit`, past their stream's
    /// offload retention at `now`, or past `floor` where that is longer; a deleted stream's are
    /// past it at once, and past the floor once that has passed.
    fn expired_offloaded(
        &self,
        now: SystemTime,
        floor: Duration,
        limit: u32,
    ) -> impl Future<Output = StoreResult<Vec<Expired>>> + Send;
}

/// Writing streams.
pub trait StreamWrite {
    /// Records that `event` happened on `node` to `segment` of `stream`, creating the stream at
    /// its first segment. `offload_retention` is how long the stream's offloaded segments are
    /// kept, as the intent its workload runs declares.
    fn record_segment(
        &mut self,
        stream: (StreamKey, Duration),
        reported: Reported<'_>,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records that the workload's stream of a kind, of no run, was deleted at `at`, by the step
    /// of an approved plan that deleted its volume: its offloaded segments are kept no longer
    /// than the operator's floor.
    fn end_stream(
        &mut self,
        stream: (Uuid, StreamKind),
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Forgets that `e` is offloaded, once its object is deleted; whether it was.
    fn forget_offloaded(&mut self, e: &Expired) -> impl Future<Output = StoreResult<bool>> + Send;
}
