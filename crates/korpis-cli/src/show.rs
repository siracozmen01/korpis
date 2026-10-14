// SPDX-License-Identifier: AGPL-3.0-or-later

//! The human form of what the API answers. `--json` prints the API's own objects instead; this
//! form is for reading, never for parsing (§2 of docs/13-surface-cli.md).

use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::value::Kind as V;
use std::fmt::Write as _;

fn status(p: &pb::Plan) -> String {
    format!("{:?}", p.status).trim_start_matches("PLAN_STATUS_").to_lowercase()
}

fn approval(p: &pb::Plan) -> String {
    format!("{:?}", p.approval).trim_start_matches("PLAN_APPROVAL_").to_lowercase()
}

fn detail(v: &pb::Value) -> String {
    match &v.kind {
        Some(V::StringValue(s)) => s.clone(),
        Some(V::IntValue(i)) => i.to_string(),
        Some(V::DoubleValue(d)) => d.to_string(),
        Some(V::BoolValue(b)) => b.to_string(),
        _ => "…".into(),
    }
}

/// A plan, as a few lines: its identifier, what it is for, what it waits for, and its steps.
#[must_use]
pub fn plan(p: &pb::Plan, workload: &str) -> String {
    let reason = p.impact.as_option().map_or("", |i| i.approval_reason.as_str());
    let mut out =
        format!("Plan {} · {workload} · {} ({}: {reason})\n", p.id, status(p), approval(p));
    for s in &p.steps {
        let details: Vec<String> =
            s.detail.iter().map(|(k, v)| format!("{k}={}", detail(v))).collect();
        let flag = if s.disruptive { "  disruptive" } else { "" };
        let gone = if s.reversible { "" } else { "  IRREVERSIBLE" };
        let _ = writeln!(
            out,
            "  {:>2}. {:<16}{}{flag}{gone}",
            s.index + 1,
            s.action,
            details.join(" ")
        );
    }
    out
}

/// One line for plans that wait: how to apply exactly them, with the flags they were planned
/// under.
#[must_use]
pub fn apply_with(paths: &[String], ids: &[String], flags: &str) -> String {
    let plans: Vec<String> = ids.iter().map(|i| format!("--plan {i}")).collect();
    let mut line = format!("apply with:  korpis apply -f {}", paths.join(" -f "));
    for part in [flags, &plans.join(" ")] {
        if !part.is_empty() {
            line.push(' ');
            line.push_str(part);
        }
    }
    line
}

/// An enum value as a person reads it: `CONVERGENCE_CONVERGED` under `CONVERGENCE_` is
/// `converged`.
#[must_use]
pub fn short(debug: impl std::fmt::Debug, prefix: &str) -> String {
    format!("{debug:?}").trim_start_matches(prefix).to_lowercase()
}

/// A timestamp in UTC, to the second, or `-` when there is none.
#[must_use]
pub fn time(at: Option<&buffa_types::google::protobuf::Timestamp>) -> String {
    let Some(at) = at else { return "-".into() };
    let (days, rem) = (at.seconds.div_euclid(86_400), at.seconds.rem_euclid(86_400));
    // Howard Hinnant's civil-from-days.
    let shifted = days + 719_468;
    let (era, doe) = (shifted.div_euclid(146_097), shifted.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let (h, min, sec) = (rem / 3600, rem % 3600 / 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{min:02}:{sec:02}Z")
}

/// Rows as aligned columns, the first row the header.
#[must_use]
pub fn table(rows: &[Vec<String>]) -> String {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..width)
        .map(|i| rows.iter().filter_map(|r| r.get(i)).map(|c| c.chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for r in rows {
        let cells: Vec<String> = r.iter().zip(&widths).map(|(c, w)| format!("{c:<w$}")).collect();
        let _ = writeln!(out, "{}", cells.join("  ").trim_end());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_is_written_in_utc() {
        let t = buffa_types::google::protobuf::Timestamp {
            seconds: 1_791_881_550,
            ..Default::default()
        };
        assert_eq!(time(Some(&t)), "2026-10-13T08:52:30Z");
        assert_eq!(time(None), "-");
        let rows = vec![vec!["ID".into(), "NAME".into()], vec!["wl_1".into(), "mc".into()]];
        assert_eq!(table(&rows), "ID    NAME\nwl_1  mc\n");
    }
}
