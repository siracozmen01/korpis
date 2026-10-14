// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis logs` (§8 of docs/13-surface-cli.md): a workload's console, read without attaching.
//! The command mints a capability token for `workload.console.read` on the one workload, with
//! the route the gateway seals with it, and opens a session at a relay (§3.1 of
//! docs/02-architecture.md); the node that runs the workload sends what its stream holds, in
//! offset order (§2 of docs/14-streams.md). Records dropped under pressure arrive as gap records
//! and offsets the node no longer holds are said to be missing, so a quiet stretch is never a
//! hole nobody mentioned (§3).
//!
//! Following, the command renews its token on the session before it expires, and if the session
//! is lost it opens another from the offset it was told to read from next, so nothing is printed
//! twice and nothing is skipped.

use super::json;
use super::session::{Minted, Session, mint, open, relay, renew};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use buffa::Message as _;
use clap::Args;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::relay_response::Message as Reply;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ACTION: &str = "workload.console.read";
/// The longest wait between attempts to open a lost session again.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// `korpis logs`.
#[derive(Debug, Clone, Args)]
pub struct Logs {
    /// The workload: an identifier, `project/name`, or a name.
    pub workload: String,
    /// Keep printing records as they are written.
    #[arg(long, short)]
    pub follow: bool,
    /// Start at this offset rather than the oldest record held.
    #[arg(long, conflicts_with_all = ["since", "tail"])]
    pub from: Option<u64>,
    /// Start at the last this many records the node holds.
    #[arg(long, conflicts_with = "since")]
    pub tail: Option<u64>,
    /// Start at the first record written this long ago or later, e.g. `1h` or `15m`.
    #[arg(long)]
    pub since: Option<String>,
    /// Only records containing this, searched on the node within its retention.
    #[arg(long)]
    pub grep: Option<String>,
    /// Read `--grep` as a regular expression.
    #[arg(long, requires = "grep")]
    pub regex: bool,
    /// Stop after this many records.
    #[arg(long)]
    pub limit: Option<u64>,
    /// Print each record's node-clock time before it.
    #[arg(long, short)]
    pub timestamps: bool,
    /// Open the session at this relay rather than the first the control plane names.
    #[arg(long)]
    pub relay: Option<String>,
}

/// What is printed: a line for a person, and the API's JSON.
pub type Out<'a> = &'a mut dyn FnMut(String, Value);

fn params(a: &Logs, from: Option<u64>) -> Result<pb::ConsoleRead, CliError> {
    let since = match (&a.since, from) {
        (Some(ago), None) => {
            let ago = korpis_core::quantity::parse_duration(ago)
                .map_err(|e| CliError::Local(format!("--since: {e}")))?;
            let at = SystemTime::now().checked_sub(ago).unwrap_or(UNIX_EPOCH);
            let secs = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
            Some(korpis_wire::timestamp(UNIX_EPOCH + Duration::from_secs(secs)))
        }
        _ => None,
    };
    Ok(pb::ConsoleRead {
        from_offset: from.or(a.from).unwrap_or(0),
        since: since.into(),
        // After a reconnection the next offset is known, and the tail is behind it.
        last: if from.is_some() { 0 } else { a.tail.unwrap_or(0) },
        follow: a.follow,
        limit: a.limit.unwrap_or(0),
        pattern: a.grep.clone().unwrap_or_default(),
        regex: a.regex,
        ..Default::default()
    })
}

/// How one session ended.
enum Ended {
    /// Normally: everything asked for was sent.
    Done,
    /// It can be opened again from where it was.
    Lost(String),
}

/// One record or missing range as a line for a person.
fn line(r: &pb::StreamRecord, timestamps: bool) -> String {
    let body = match (r.kind.as_known(), r.gap.as_option()) {
        (Some(pb::StreamRecordKind::STREAM_RECORD_KIND_GAP), Some(g)) => format!(
            "\u{2500}\u{2500} {} records ({} bytes) dropped over {:.1}s: {} \u{2500}\u{2500}",
            g.records,
            g.bytes,
            Duration::from_millis(g.millis).as_secs_f64(),
            g.reason
        ),
        _ => String::from_utf8_lossy(&r.body).trim_end_matches(['\n', '\r']).to_owned(),
    };
    if timestamps { format!("{} {body}", crate::show::time(r.time.as_option())) } else { body }
}

fn missing(range: &pb::OffsetRange) -> String {
    format!(
        "\u{2500}\u{2500} offsets {} to {} are no longer held on the node \u{2500}\u{2500}",
        range.first,
        range.end.saturating_sub(1)
    )
}

/// Prints what `session` sends until it ends, renewing its token when it must; the offset to
/// read from next is kept in `next`.
async fn print(
    c: &Connection,
    workload: &str,
    (session, mut minted): (&mut Session, Minted),
    (a, next): (&Logs, &mut Option<u64>),
    out: Out<'_>,
) -> Result<Ended, CliError> {
    loop {
        let message = tokio::select! {
            m = session.message::<pb::RelayResponse>() => m,
            () = tokio::time::sleep(minted.renew_in()), if a.follow => {
                renew(c, workload, session, &mut minted).await?;
                continue;
            }
        };
        let frame = match message {
            Ok(Some(m)) => match m.to_owned_message().message {
                Some(Reply::Frame(f)) => f.frame,
                _ => None,
            },
            Ok(None) => return Ok(Ended::Lost("the relay closed the session".into())),
            Err(e) => return Ok(Ended::Lost(CliError::from(e).to_string())),
        };
        match frame {
            Some(Frame::Data(bytes)) => {
                let chunk = pb::ConsoleChunk::decode_from_slice(&bytes)
                    .map_err(|e| CliError::Transport(format!("a console chunk: {e}")))?;
                if let Some(range) = chunk.missing.as_option() {
                    out(missing(range), json(range));
                }
                for r in &chunk.records {
                    out(line(r, a.timestamps), json(r));
                }
                *next = Some(chunk.next_offset);
            }
            Some(Frame::End(end)) => return ended(&end),
            _ => {}
        }
    }
}

fn ended(end: &pb::SessionEnd) -> Result<Ended, CliError> {
    match end.code.as_str() {
        "" => Ok(Ended::Done),
        "expired" | "unavailable" => Ok(Ended::Lost(format!("{}: {}", end.code, end.message))),
        code => Err(CliError::Local(format!("the session ended: {code}: {}", end.message))),
    }
}

/// Prints the workload's console as `a` asks, handing each line to `out`.
pub async fn logs(c: &Connection, a: &Logs, out: Out<'_>) -> Result<(), CliError> {
    let workload = resolve::workload(c, &a.workload).await?;
    let at = relay(c, a.relay.as_deref()).await?;
    let mut next: Option<u64> = None;
    let mut backoff = Duration::from_secs(1);
    loop {
        let minted = mint(c, &workload, ACTION).await?;
        let read = params(a, next)?;
        let lost = match open(&at, &workload, &minted, read.encode_to_vec()).await {
            Ok(mut session) => {
                let ended =
                    print(c, &workload, (&mut session, minted), (a, &mut next), out).await?;
                match ended {
                    Ended::Done => return Ok(()),
                    Ended::Lost(why) => why,
                }
            }
            Err(e) => e.to_string(),
        };
        if !a.follow {
            return Err(CliError::Transport(format!("the console session was lost: {lost}")));
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gap_and_a_missing_range_say_what_was_lost() {
        let gap = pb::StreamRecord {
            kind: pb::StreamRecordKind::STREAM_RECORD_KIND_GAP.into(),
            gap: pb::StreamGap {
                records: 41_208,
                bytes: 9_000_000,
                millis: 6_000,
                reason: "write rate exceeded the stream's limit".into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        assert_eq!(
            line(&gap, false),
            "\u{2500}\u{2500} 41208 records (9000000 bytes) dropped over 6.0s: write rate \
             exceeded the stream's limit \u{2500}\u{2500}"
        );
        let range = pb::OffsetRange { first: 0, end: 100, ..Default::default() };
        assert!(missing(&range).contains("offsets 0 to 99 are no longer held"));
        let text = pb::StreamRecord { body: b"Saved the game\n".to_vec(), ..Default::default() };
        assert_eq!(line(&text, false), "Saved the game");
    }
}
