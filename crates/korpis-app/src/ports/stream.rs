// SPDX-License-Identifier: AGPL-3.0-or-later

//! Streams' segment index (§4 of docs/14-streams.md): kept from what nodes journal, read to say
//! where a stream's records are and what is held only on a node.

use super::StoreResult;
use korpis_core::workload::stream::{Segment, SegmentEvent, Stream, StreamKind};
use std::future::Future;
use uuid::Uuid;

/// Reading streams.
pub trait StreamRead {
    /// The workload's stream of `kind`, with its index; none before its first segment.
    fn stream(
        &self,
        workload: Uuid,
        kind: StreamKind,
    ) -> impl Future<Output = StoreResult<Option<Stream>>> + Send;
}

/// Writing streams.
pub trait StreamWrite {
    /// Records that `event` happened on `node` to `segment` of `stream`, a workload's stream of
    /// a kind, creating the stream at its first segment.
    fn record_segment(
        &mut self,
        stream: (Uuid, StreamKind),
        event: SegmentEvent,
        segment: &Segment,
        node: Uuid,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
