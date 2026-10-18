// SPDX-License-Identifier: AGPL-3.0-or-later

//! Streams: `korpis.v1.StreamSpec`, `Stream`, and `StreamSegment` (§3.6 of docs/01-model.md,
//! docs/14-streams.md). A limit left zero is the default, so an intent stored before streams had
//! limits reads as one with the defaults.

use crate::{Result, duration, duration_from, invalid, time, timestamp, wire_id};
use korpis_core::id::IdKind;
use korpis_core::workload::spec::StreamSpec;
use korpis_core::workload::spec::streams::Retention;
use korpis_core::workload::stream::{Segment, SegmentEvent, Stream, StreamKind};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::stream_spec::Retention as R;

/// The wire form of stream limits.
#[must_use]
pub fn spec_to_pb(s: &StreamSpec) -> pb::StreamSpec {
    pb::StreamSpec {
        rate_bytes_per_sec: s.rate,
        burst_bytes: s.burst,
        retention: Some(match s.retention {
            Retention::Bytes(b) => R::RetentionBytes(b),
            Retention::Duration(d) => R::RetentionDuration(Box::new(duration(d))),
        }),
        offload_retention: duration(s.offload_retention).into(),
        ..Default::default()
    }
}

/// Stream limits from the field `field`, each zero or absent part the default.
pub fn spec_from_pb(field: &str, s: Option<&pb::StreamSpec>) -> Result<StreamSpec> {
    let default = StreamSpec::default();
    let Some(s) = s else { return Ok(default) };
    let or = |v: u64, d: u64| if v == 0 { d } else { v };
    let retention = match &s.retention {
        None | Some(R::RetentionBytes(0)) => default.retention,
        Some(R::RetentionBytes(b)) => Retention::Bytes(*b),
        Some(R::RetentionDuration(d)) => {
            Retention::Duration(duration_from(&format!("{field}.retention_duration"), d)?)
        }
    };
    let offload_retention = match s.offload_retention.as_option() {
        None => default.offload_retention,
        Some(d) => match duration_from(&format!("{field}.offload_retention"), d)? {
            d if d.is_zero() => default.offload_retention,
            d => d,
        },
    };
    Ok(StreamSpec {
        rate: or(s.rate_bytes_per_sec, default.rate),
        burst: or(s.burst_bytes, default.burst),
        retention,
        offload_retention,
    })
}

/// The wire form of a stream's kind.
#[must_use]
pub const fn kind_to_pb(k: StreamKind) -> pb::StreamKind {
    match k {
        StreamKind::Console => pb::StreamKind::STREAM_KIND_CONSOLE,
        StreamKind::Log => pb::StreamKind::STREAM_KIND_LOG,
        StreamKind::Event => pb::StreamKind::STREAM_KIND_EVENT,
        StreamKind::Metric => pb::StreamKind::STREAM_KIND_METRIC,
    }
}

/// A stream's kind from the field `field`.
pub fn kind_from_pb(field: &str, k: buffa::EnumValue<pb::StreamKind>) -> Result<StreamKind> {
    match k.as_known() {
        Some(pb::StreamKind::STREAM_KIND_CONSOLE) => Ok(StreamKind::Console),
        Some(pb::StreamKind::STREAM_KIND_LOG) => Ok(StreamKind::Log),
        Some(pb::StreamKind::STREAM_KIND_EVENT) => Ok(StreamKind::Event),
        Some(pb::StreamKind::STREAM_KIND_METRIC) => Ok(StreamKind::Metric),
        _ => Err(invalid(field, "console, log, event, or metric")),
    }
}

/// What happened to a segment, from the field `field`.
pub fn event_from_pb(
    field: &str,
    e: buffa::EnumValue<agent::SegmentEvent>,
) -> Result<SegmentEvent> {
    match e.as_known() {
        Some(agent::SegmentEvent::SEGMENT_EVENT_SEALED) => Ok(SegmentEvent::Sealed),
        Some(agent::SegmentEvent::SEGMENT_EVENT_OFFLOADED) => Ok(SegmentEvent::Offloaded),
        Some(agent::SegmentEvent::SEGMENT_EVENT_PRUNED) => Ok(SegmentEvent::Pruned),
        _ => Err(invalid(field, "sealed, offloaded, or pruned")),
    }
}

/// The wire form of a segment.
#[must_use]
pub fn segment_to_pb(s: &Segment) -> pb::StreamSegment {
    pb::StreamSegment {
        first_offset: s.first_offset,
        end_offset: s.end_offset,
        first_at: timestamp(s.first_at).into(),
        last_at: timestamp(s.last_at).into(),
        bytes: s.bytes,
        digest: s.digest.clone(),
        node: s.node.map(|n| wire_id(IdKind::Node, n)).unwrap_or_default(),
        offloaded: s.offloaded(),
        repository: s.repository.map(|r| wire_id(IdKind::Repository, r)).unwrap_or_default(),
        ..Default::default()
    }
}

/// A segment a node reports, from the field `field`; its node is the journal's, set by the
/// caller.
pub fn segment_from_pb(field: &str, s: &pb::StreamSegment) -> Result<Segment> {
    if s.end_offset <= s.first_offset {
        return Err(invalid(field, "a segment holds at least one record"));
    }
    let at = |name: &str, t: Option<&buffa_types::google::protobuf::Timestamp>| {
        t.map(|t| time(&format!("{field}.{name}"), t))
            .transpose()
            .map(|t| t.unwrap_or(std::time::UNIX_EPOCH))
    };
    Ok(Segment {
        first_offset: s.first_offset,
        end_offset: s.end_offset,
        first_at: at("first_at", s.first_at.as_option())?,
        last_at: at("last_at", s.last_at.as_option())?,
        bytes: s.bytes,
        digest: s.digest.clone(),
        node: None,
        repository: match s.repository.as_str() {
            "" => None,
            r => Some(crate::id(IdKind::Repository, &format!("{field}.repository"), r)?),
        },
    })
}

/// The wire form of a stream and its index.
#[must_use]
pub fn stream_to_pb(s: &Stream) -> pb::Stream {
    pb::Stream {
        // Nil until the index first hears of the stream, at its first sealed segment.
        id: if s.id.is_nil() { String::new() } else { wire_id(IdKind::Stream, s.id) },
        workload: wire_id(IdKind::Workload, s.workload),
        kind: kind_to_pb(s.kind).into(),
        run: s.run.map(|r| wire_id(IdKind::Run, r)).unwrap_or_default(),
        node_only_bytes: s.node_only_bytes(),
        segments: s.segments.iter().map(segment_to_pb).collect(),
        ..Default::default()
    }
}
