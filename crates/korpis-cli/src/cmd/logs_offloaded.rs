// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis logs --offloaded` (§8 of docs/13-surface-cli.md): a workload's console as the
//! repository that takes its stream holds it, read through the control plane rather than from
//! the node (§4 of docs/14-streams.md). For a workload whose node is gone, or that was retired.
//! Offsets no offloaded segment holds are said to be missing; what the node has not offloaded
//! yet is not here, and the last line says where offloaded records end.

use super::json;
use super::logs::{Logs, Out, line};
use crate::connect::Connection;
use crate::error::CliError;
use korpis_proto::korpis::v1 as pb;

fn missing(range: &pb::OffsetRange) -> String {
    format!(
        "\u{2500}\u{2500} offsets {} to {} are in no offloaded segment \u{2500}\u{2500}",
        range.first,
        range.end.saturating_sub(1)
    )
}

/// Prints `workload`'s offloaded console as `a` asks, handing each line to `out`.
pub async fn logs(
    c: &Connection,
    (workload, a): (&str, &Logs),
    out: Out<'_>,
) -> Result<(), CliError> {
    let client = pb::StreamServiceClient::new(c.http(), c.config());
    let from = a.from.unwrap_or(0);
    let r = pb::ReadOffloadedRequest {
        workload: workload.to_owned(),
        from_offset: from,
        limit: a.limit.unwrap_or(0),
        ..Default::default()
    };
    let mut stream = client.read_offloaded(r).await?;
    while let Some(chunk) = stream.message().await? {
        let chunk = chunk.to_owned_message();
        if let Some(range) = chunk.missing.as_option() {
            out(missing(range), json(range));
        }
        for r in &chunk.records {
            out(line(r, a.timestamps), json(r));
        }
        if chunk.caught_up {
            let end = if chunk.next_offset > from {
                format!("offloaded through offset {}", chunk.next_offset - 1)
            } else {
                format!("nothing offloaded from offset {from}")
            };
            out(format!("\u{2500}\u{2500} {end} \u{2500}\u{2500}"), json(&chunk));
        }
    }
    Ok(())
}
