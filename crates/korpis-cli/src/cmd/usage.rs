// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis get usage` and `korpis get periods`: what metering recorded (§2 of
//! docs/15-observability.md), each figure with the window it covers and how it was measured.

use super::{Answer, json};
use crate::complete;
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{bytes, short, table, time};
use clap_complete::engine::ArgValueCompleter;
use korpis_proto::korpis::v1 as pb;
use std::fmt::Write as _;
use std::time::SystemTime;

/// How each figure of `korpis get usage` was measured, printed under its table.
const METHOD: &str = "Times are UTC. CPU, throttled time, and block and network bytes are the \
                      kernel's counters, differenced between two samples on the node; memory is \
                      integrated from readings every ten seconds; disk is what the volumes' \
                      filesystem accounted at the interval's end. Egress, and each endpoint whose \
                      accounting is on, are counted in the node's host table: egress is what left \
                      the workload's link for anything but another workload's on its node. A gap \
                      is an interval nothing was heard over.";

/// What `korpis get usage` reads.
#[derive(Debug, Clone, clap::Args)]
pub struct Usage {
    /// Only this workload's: an identifier, `project/name`, or a name.
    #[arg(add = ArgValueCompleter::new(complete::workloads))]
    #[arg(long, conflicts_with_all = ["project", "organization"])]
    workload: Option<String>,
    /// Every workload's of this project.
    #[arg(add = ArgValueCompleter::new(complete::projects))]
    #[arg(long, conflicts_with = "organization")]
    project: Option<String>,
    /// Every workload's in this organization's subtree; the top one you can read by default.
    #[arg(long = "org", add = ArgValueCompleter::new(complete::organizations))]
    organization: Option<String>,
    /// Which intervals: those beginning in the window.
    #[command(flatten)]
    window: Window,
    /// Superseded records too: the gaps a later measurement replaced.
    #[arg(long)]
    every_version: bool,
    /// As an invoice sums them: the UTC days from the one `--from` falls in and before the one
    /// `--to` falls in, each record in the day it counts in, an adjustment in the day it was
    /// recorded; a closed day reads the same whenever it is read.
    #[arg(long)]
    by_period: bool,
    /// At most this many, the earliest first.
    #[arg(long, default_value_t = 100)]
    limit: usize,
}

/// What `korpis get periods` reads.
#[derive(Debug, Clone, clap::Args)]
pub struct Periods {
    /// Which days: those beginning in the window.
    #[command(flatten)]
    window: Window,
}

/// A window of time, as a person names its ends.
#[derive(Debug, Clone, clap::Args)]
pub struct Window {
    /// From this: RFC 3339, a UTC day such as `2026-10-01`, or a span ago such as `7d`; a day
    /// before `--to` when absent.
    #[arg(long)]
    from: Option<String>,
    /// Up to this, as for `--from`; now when absent.
    #[arg(long)]
    to: Option<String>,
}

type Stamp = buffa_types::google::protobuf::Timestamp;

/// The instant `text` names at `now`: an RFC 3339 timestamp, the start of a UTC day, since
/// metering's days are UTC days, or a span before `now`.
fn instant(text: &str, now: SystemTime) -> Result<Stamp, CliError> {
    if let Ok(t) = text.parse::<jiff::Timestamp>() {
        return Ok(korpis_wire::timestamp(t.into()));
    }
    let bad = |e: &dyn std::fmt::Display| {
        CliError::Local(format!("{text:?}: RFC 3339, a day such as 2026-10-01, or a span ({e})"))
    };
    if let Ok(day) = text.parse::<jiff::civil::Date>() {
        let at = day.to_zoned(jiff::tz::TimeZone::UTC).map_err(|e| bad(&e))?;
        return Ok(korpis_wire::timestamp(at.timestamp().into()));
    }
    let ago = korpis_core::quantity::parse_duration(text).map_err(|e| bad(&e))?;
    Ok(korpis_wire::timestamp(now.checked_sub(ago).unwrap_or(SystemTime::UNIX_EPOCH)))
}

/// The window's two ends, absent where the control plane's default stands.
fn ends(w: &Window) -> Result<(Option<Stamp>, Option<Stamp>), CliError> {
    let now = SystemTime::now();
    let from = w.from.as_deref().map(|t| instant(t, now)).transpose()?;
    let to = w.to.as_deref().map(|t| instant(t, now)).transpose()?;
    Ok((from, to))
}

/// What `get usage` names: a workload or a project, or else an organization, the caller's own
/// top one when nothing is named.
async fn request(c: &Connection, u: &Usage) -> Result<pb::ListUsageRequest, CliError> {
    let (from, to) = ends(&u.window)?;
    let mut request = pb::ListUsageRequest {
        from: from.into(),
        to: to.into(),
        every_version: u.every_version,
        by_period: u.by_period,
        ..Default::default()
    };
    if let Some(w) = &u.workload {
        request.workload = Some(resolve::workload(c, w).await?);
    } else if let Some(p) = &u.project {
        request.project = Some(resolve::project(c, p).await?);
    } else {
        request.organization = Some(resolve::organization(c, u.organization.as_deref()).await?);
    }
    Ok(request)
}

/// The records `u` names, following page tokens until `u.limit` of them were read.
pub async fn usage(c: &Connection, u: &Usage) -> Result<Answer, CliError> {
    let mut request = request(c, u).await?;
    let client = pb::MeteringServiceClient::new(c.http(), c.config());
    let mut list = Vec::new();
    while list.len() < u.limit {
        let size = u32::try_from(u.limit - list.len()).unwrap_or(u32::MAX).min(1000);
        let token = std::mem::take(&mut request.page).into_option().unwrap_or_default();
        request.page = pb::PageRequest { page_size: size, ..token }.into();
        let page = client.list_usage(request.clone()).await?.into_owned();
        list.extend(page.records);
        let next = page.page.into_option().unwrap_or_default().next_page_token;
        if next.is_empty() {
            break;
        }
        request.page = pb::PageRequest { page_token: next, ..Default::default() }.into();
    }
    list.truncate(u.limit);
    let mut human = table(&usage_rows(&list));
    let endpoints = endpoint_rows(&list);
    if endpoints.len() > 1 {
        let _ = write!(human, "\n{}", table(&endpoints));
    }
    let _ = writeln!(human, "\n{METHOD}");
    Ok(Answer { json: json(&list), human })
}

fn usage_rows(list: &[pb::UsageRecord]) -> Vec<Vec<String>> {
    let header = [
        "FROM",
        "TO",
        "WORKLOAD",
        "KIND",
        "CPU",
        "THROTTLED",
        "MEMORY",
        "PEAK",
        "READ",
        "WRITTEN",
        "RECEIVED",
        "SENT",
        "EGRESS",
        "DISK",
        "NODE",
        "CAUSE",
    ];
    let mut rows = vec![header.iter().map(|h| (*h).to_owned()).collect()];
    rows.extend(list.iter().map(usage_row));
    rows
}

fn usage_row(r: &pb::UsageRecord) -> Vec<String> {
    let size = |n: Option<u64>| n.map_or_else(|| "-".to_owned(), bytes);
    let seconds = |n: Option<u64>| n.map_or_else(|| "-".to_owned(), cpu);
    let mut cause = if r.cause.is_empty() { "-".to_owned() } else { r.cause.clone() };
    if !r.losses.is_empty() {
        let _ = write!(cause, ", {} span(s) not measured", r.losses.len());
    }
    vec![
        time(r.from.as_option()),
        time(r.to.as_option()),
        r.workload.clone(),
        short(r.kind, "USAGE_KIND_"),
        seconds(r.cpu_usec),
        seconds(r.cpu_throttled_usec),
        r.memory_kib_seconds.map_or_else(|| "-".to_owned(), memory),
        size(r.memory_peak_bytes),
        size(r.io_read_bytes),
        size(r.io_write_bytes),
        size(r.net_rx_bytes),
        size(r.net_tx_bytes),
        size(r.egress_bytes),
        size(r.disk_bytes),
        r.node.clone().unwrap_or_else(|| "-".to_owned()),
        cause,
    ]
}

/// What each counted endpoint of each record carried, a row each, with its header; the header
/// alone where no record counts an endpoint.
fn endpoint_rows(list: &[pb::UsageRecord]) -> Vec<Vec<String>> {
    let header = ["FROM", "TO", "WORKLOAD", "ENDPOINT", "RECEIVED", "SENT", "PACKETS IN", "OUT"];
    let mut rows = vec![header.iter().map(|h| (*h).to_owned()).collect()];
    for r in list {
        for e in &r.endpoints {
            rows.push(vec![
                time(r.from.as_option()),
                time(r.to.as_option()),
                r.workload.clone(),
                e.endpoint.clone(),
                bytes(e.received_bytes),
                bytes(e.sent_bytes),
                e.received_packets.to_string(),
                e.sent_packets.to_string(),
            ]);
        }
    }
    rows
}

/// Microseconds of CPU time as seconds, to the millisecond.
fn cpu(usec: u64) -> String {
    format!("{}.{:03} s", usec / 1_000_000, usec % 1_000_000 / 1000)
}

/// KiB-seconds of memory as GiB-hours, to the thousandth: what a GiB held for an hour is.
fn memory(kib_seconds: u64) -> String {
    const GIB_HOUR: u128 = 1024 * 1024 * 3600;
    let thousandths = u128::from(kib_seconds) * 1000 / GIB_HOUR;
    format!("{}.{:03} GiB-h", thousandths / 1000, thousandths % 1000)
}

/// The metering days `p` names, and whether each has closed.
pub async fn periods(c: &Connection, p: &Periods) -> Result<Answer, CliError> {
    let (from, to) = ends(&p.window)?;
    let request = pb::ListPeriodsRequest { from: from.into(), to: to.into(), ..Default::default() };
    let client = pb::MeteringServiceClient::new(c.http(), c.config());
    let list = client.list_periods(request).await?.into_owned().periods;
    let mut rows = vec![vec!["DAY".to_owned(), "STATE".to_owned(), "CLOSES AT".to_owned()]];
    rows.extend(list.iter().map(|d| {
        let state = short(d.state, "PERIOD_STATE_");
        vec![time(d.start.as_option()), state, time(d.closes_at.as_option())]
    }));
    Ok(Answer { json: json(&list), human: table(&rows) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_end_is_rfc_3339_a_utc_day_or_a_span_ago() {
        let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(20_000 * 86_400);
        assert_eq!(instant("2026-10-01T12:00:00Z", now).unwrap().seconds, 1_790_856_000);
        assert_eq!(instant("2026-10-01", now).unwrap().seconds, 1_790_812_800);
        assert_eq!(instant("1d", now).unwrap().seconds, 19_999 * 86_400);
        assert!(instant("yesterday", now).is_err());
    }

    #[test]
    fn a_figure_is_shown_in_its_unit_and_an_absent_one_is_not_zero() {
        assert_eq!(cpu(1_234_567), "1.234 s");
        assert_eq!(memory(1024 * 1024 * 3600 * 3 / 2), "1.500 GiB-h");
        let row = usage_row(&pb::UsageRecord { cpu_usec: Some(0), ..Default::default() });
        assert_eq!(row.get(4).map(String::as_str), Some("0.000 s"));
        assert_eq!(row.get(5).map(String::as_str), Some("-"), "throttling was not measured");
        let game = pb::EndpointUsage {
            endpoint: "game".into(),
            received_bytes: 2048,
            sent_packets: 6,
            ..Default::default()
        };
        let r = pb::UsageRecord {
            egress_bytes: Some(4096),
            endpoints: vec![game],
            ..Default::default()
        };
        assert_eq!(usage_row(&r).get(12).map(String::as_str), Some(bytes(4096).as_str()));
        let rows = endpoint_rows(&[r]);
        assert_eq!((rows.len(), rows[1][3].as_str(), rows[1][7].as_str()), (2, "game", "6"));
        assert_eq!(endpoint_rows(&[]).len(), 1, "the header alone, which is not printed");
    }
}
