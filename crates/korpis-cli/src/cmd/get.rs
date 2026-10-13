// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis get` and `korpis status`: what the caller may read, listed or in detail.

use super::{Answer, Get, json};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::subject::Kind as S;
use std::fmt::Write as _;

fn subject(s: Option<&pb::Subject>) -> String {
    match s.and_then(|s| s.kind.as_ref()) {
        Some(S::User(id) | S::Token(id) | S::Workload(id) | S::Extension(id)) => id.clone(),
        Some(S::External(_)) => "external".into(),
        Some(S::System(_)) => "system".into(),
        None => "-".into(),
    }
}

fn rows<T>(header: &[&str], items: &[T], row: impl Fn(&T) -> Vec<String>) -> String {
    let mut all = vec![header.iter().map(|h| (*h).to_owned()).collect()];
    all.extend(items.iter().map(row));
    table(&all)
}

fn workload_row(s: &pb::WorkloadStatus) -> Vec<String> {
    let w = s.workload.as_option().cloned().unwrap_or_default();
    let observed =
        s.observation.as_option().map_or("-".into(), |o| short(o.state, "OBSERVED_STATE_"));
    let node = s.placement.as_option().map_or("-".into(), |p| p.node.clone());
    vec![w.id, w.name, short(s.convergence, "CONVERGENCE_"), observed, node]
}

async fn effects(
    c: &Connection,
    workload: Option<&String>,
    plan: Option<&String>,
    node: Option<&String>,
) -> Result<Answer, CliError> {
    let workload = match workload {
        Some(w) => Some(resolve::workload(c, w).await?),
        None => None,
    };
    let request = pb::ListEffectsRequest {
        workload,
        plan: plan.cloned(),
        node: node.cloned(),
        ..Default::default()
    };
    let client = pb::EffectServiceClient::new(c.http(), c.config());
    let list = client.list_effects(request).await?.into_owned().effects;
    let header = ["AT", "ACTION", "OUTCOME", "ACTOR", "GRANT"];
    let human = rows(&header, &list, |e| {
        let outcome = short(e.outcome, "EFFECT_OUTCOME_");
        let grant = e.grant.clone().unwrap_or_else(|| "-".into());
        vec![time(e.at.as_option()), e.action.clone(), outcome, subject(e.actor.as_option()), grant]
    });
    Ok(Answer { json: json(&list), human })
}

/// Answers `korpis get`.
pub async fn get(c: &Connection, what: &Get) -> Result<Answer, CliError> {
    match what {
        Get::Organizations => {
            let list = resolve::organizations(c).await?;
            let human = rows(&["ID", "NAME", "PARENT"], &list, |o| {
                vec![o.id.clone(), o.name.clone(), o.parent.clone().unwrap_or_else(|| "-".into())]
            });
            Ok(Answer { json: json(&list), human })
        }
        Get::Projects { organization } => {
            let org = resolve::organization(c, organization.as_deref()).await?;
            let list = resolve::projects(c, &org).await?;
            let human = rows(&["ID", "NAME"], &list, |p| vec![p.id.clone(), p.name.clone()]);
            Ok(Answer { json: json(&list), human })
        }
        Get::Workloads { project } => {
            let list = resolve::workloads(c, &resolve::project(c, project).await?).await?;
            let header = ["ID", "NAME", "CONVERGENCE", "OBSERVED", "NODE"];
            Ok(Answer { json: json(&list), human: rows(&header, &list, workload_row) })
        }
        Get::Nodes { organization } => {
            let org = resolve::organization(c, organization.as_deref()).await?;
            let request = pb::ListNodesRequest { organization: org, ..Default::default() };
            let client = pb::NodeServiceClient::new(c.http(), c.config());
            let list = client.list_nodes(request).await?.into_owned().nodes;
            let human = rows(&["ID", "NAME", "STATUS", "AGENT", "LAST SEEN"], &list, |n| {
                let status = short(n.status, "NODE_STATUS_");
                vec![
                    n.id.clone(),
                    n.name.clone(),
                    status,
                    n.agent_version.clone(),
                    time(n.last_seen.as_option()),
                ]
            });
            Ok(Answer { json: json(&list), human })
        }
        Get::Plans { workload } => {
            let workload = resolve::workload(c, workload).await?;
            let request = pb::ListPlansRequest { workload, ..Default::default() };
            let client = pb::PlanServiceClient::new(c.http(), c.config());
            let list = client.list_plans(request).await?.into_owned().plans;
            let human =
                rows(&["ID", "KIND", "STATUS", "APPROVAL", "STEPS", "COMPUTED"], &list, |p| {
                    vec![
                        p.id.clone(),
                        short(p.kind, "PLAN_KIND_"),
                        short(p.status, "PLAN_STATUS_"),
                        short(p.approval, "PLAN_APPROVAL_"),
                        p.steps.len().to_string(),
                        time(p.computed_at.as_option()),
                    ]
                });
            Ok(Answer { json: json(&list), human })
        }
        Get::Effects { workload, plan, node } => {
            effects(c, workload.as_ref(), plan.as_ref(), node.as_ref()).await
        }
    }
}

/// Why nothing holds a workload, as the scheduler explained it to the caller.
fn unplaced(out: &mut String, s: &pb::WorkloadStatus) {
    let Some(u) = s.unplaced.as_option() else { return };
    let _ = writeln!(out, "unplaced since {}:", time(u.since.as_option()));
    for f in u.explanation.as_option().map(|e| e.filtered.as_slice()).unwrap_or_default() {
        let _ = writeln!(out, "  {}  {}: {}", f.node, f.check, f.detail);
    }
}

/// Answers `korpis status`: where the workload is, what it is doing, and what it waits for.
pub async fn status(c: &Connection, workload: &str) -> Result<Answer, CliError> {
    let id = resolve::workload(c, workload).await?;
    let request = pb::GetWorkloadRequest { id, ..Default::default() };
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let s =
        client.get_workload(request).await?.into_owned().status.into_option().unwrap_or_default();
    let w = s.workload.as_option().cloned().unwrap_or_default();
    let mut out = format!("{} ({})  {}\n", w.name, w.id, short(s.convergence, "CONVERGENCE_"));
    if let Some(p) = s.placement.as_option() {
        let _ = writeln!(out, "node:      {} since {}", p.node, time(p.bound_at.as_option()));
    }
    if let Some(o) = s.observation.as_option() {
        let since = time(o.since.as_option());
        let _ = writeln!(
            out,
            "observed:  {} since {since}, runtime {}",
            short(o.state, "OBSERVED_STATE_"),
            o.runtime_id
        );
        if !o.reason_detail.is_empty() {
            let _ = writeln!(out, "reason:    {}", o.reason_detail);
        }
    }
    if let Some(p) = s.active_plan.as_option() {
        let _ = writeln!(out, "plan:      {} {}", p.id, short(p.status, "PLAN_STATUS_"));
    }
    unplaced(&mut out, &s);
    Ok(Answer { json: json(&s), human: out })
}
