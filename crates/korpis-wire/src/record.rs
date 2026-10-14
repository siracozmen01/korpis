// SPDX-License-Identifier: AGPL-3.0-or-later

//! A stream's records as the API sends them (§2 of docs/14-streams.md): from a node reading its
//! segment files, or from the control plane reading offloaded ones.

use buffa_types::google::protobuf::Timestamp;
use korpis_proto::korpis::v1 as pb;
use korpis_stream::record::{Gap, Kind, Record};

/// The wire form of `r`.
#[must_use]
pub fn record_to_pb(r: Record) -> pb::StreamRecord {
    let kind = match r.kind {
        Kind::Text => pb::StreamRecordKind::STREAM_RECORD_KIND_TEXT,
        Kind::Structured => pb::StreamRecordKind::STREAM_RECORD_KIND_STRUCTURED,
        Kind::Gap => pb::StreamRecordKind::STREAM_RECORD_KIND_GAP,
        Kind::Control => pb::StreamRecordKind::STREAM_RECORD_KIND_CONTROL,
    };
    let gap = (r.kind == Kind::Gap).then(|| Gap::parse(&r.body)).flatten().map(|g| pb::StreamGap {
        records: g.records,
        bytes: g.bytes,
        millis: g.millis,
        reason: g.reason,
        ..Default::default()
    });
    pb::StreamRecord {
        offset: r.offset,
        time: Timestamp {
            seconds: r.time_nanos.div_euclid(1_000_000_000),
            nanos: i32::try_from(r.time_nanos.rem_euclid(1_000_000_000)).unwrap_or(0),
            ..Default::default()
        }
        .into(),
        source: r.source.name(),
        kind: kind.into(),
        body: r.body,
        gap: gap.into(),
        ..Default::default()
    }
}
