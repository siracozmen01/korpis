// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis stream` (§8 of docs/13-surface-cli.md): a workload's console stream as the control
//! plane indexes it (§4 of docs/14-streams.md): how much its node keeps, how long its offloaded
//! segments are kept and the operator's floor beneath that, which every tenant is shown (§8 of
//! docs/17-security.md), how much is held only on the node, and which offsets are lost.

use super::{Answer, json};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::bytes;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::stream::Retention;

/// A duration as a person reads it: in days where it is whole days.
fn span(d: Option<&buffa_types::google::protobuf::Duration>) -> String {
    let seconds = d.map_or(0, |d| d.seconds);
    match seconds {
        0 => "none".into(),
        s if s % 86_400 == 0 => format!("{}d", s / 86_400),
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s => format!("{s}s"),
    }
}

/// The stream as lines for a person.
fn shown(s: &pb::Stream, lost: &[pb::OffsetRange]) -> String {
    let node = match &s.retention {
        Some(Retention::RetentionBytes(b)) => bytes(*b),
        Some(Retention::RetentionDuration(d)) => span(Some(d)),
        None => "the default".into(),
    };
    let offloaded = s.segments.iter().filter(|g| g.offloaded).count();
    let lost = if lost.is_empty() {
        "nothing".into()
    } else {
        let ranges = lost.iter().map(|r| format!("{} to {}", r.first, r.end.saturating_sub(1)));
        ranges.collect::<Vec<_>>().join(", ")
    };
    [
        format!("kept on the node     {node}"),
        format!("offloaded, kept for  {}", span(s.offload_retention.as_option())),
        format!("operator's floor     {}", span(s.retention_floor.as_option())),
        format!("held only on node    {}", bytes(s.node_only_bytes)),
        format!("segments             {} ({offloaded} offloaded)", s.segments.len()),
        format!("offsets lost         {lost}"),
    ]
    .join("\n")
}

/// Answers `korpis stream WORKLOAD`.
///
/// # Errors
/// The workload is not found, or the call failed.
pub async fn stream(c: &Connection, workload: &str) -> Result<Answer, CliError> {
    let id = resolve::workload(c, workload).await?;
    let client = pb::StreamServiceClient::new(c.http(), c.config());
    let r = pb::GetStreamRequest { workload: id, ..Default::default() };
    let got = client.get_stream(r).await?.into_owned();
    let s = got.stream.into_option().unwrap_or_default();
    Ok(Answer { human: shown(&s, &got.lost), json: json(&s) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_says_what_is_kept_where_and_what_is_lost() {
        let days = |n: i64| buffa_types::google::protobuf::Duration {
            seconds: n * 86_400,
            ..Default::default()
        };
        let s = pb::Stream {
            retention: Some(Retention::RetentionBytes(64 << 20)),
            offload_retention: days(30).into(),
            retention_floor: days(0).into(),
            node_only_bytes: 4096,
            segments: vec![pb::StreamSegment { offloaded: true, ..Default::default() }],
            ..Default::default()
        };
        let lost = [pb::OffsetRange { first: 0, end: 10, ..Default::default() }];
        let text = shown(&s, &lost);
        assert!(text.contains("offloaded, kept for  30d"), "{text}");
        assert!(text.contains("operator's floor     none"), "{text}");
        assert!(text.contains("1 (1 offloaded)"), "{text}");
        assert!(text.contains("offsets lost         0 to 9"), "{text}");
    }
}
