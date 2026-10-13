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
        let _ = writeln!(out, "  {:>2}. {:<16}{}{flag}", s.index + 1, s.action, details.join(" "));
    }
    out
}

/// One line for a plan that waits: how to apply it.
#[must_use]
pub fn apply_with(paths: &[String], ids: &[String]) -> String {
    let plans: Vec<String> = ids.iter().map(|i| format!("--plan {i}")).collect();
    format!("apply with:  korpis apply -f {} {}", paths.join(" -f "), plans.join(" "))
}
