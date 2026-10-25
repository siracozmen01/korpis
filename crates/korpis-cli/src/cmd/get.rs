// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis get` and `korpis status`: what the caller may read, listed or in detail.

use super::{Answer, Effects, Get, json};
use crate::connect::Connection;
use crate::error::CliError;
use crate::resolve;
use crate::show::{short, table, time};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::subject::Kind as S;
use std::fmt::Write as _;

pub(crate) fn subject(s: Option<&pb::Subject>) -> String {
    match s.and_then(|s| s.kind.as_ref()) {
        Some(S::User(id) | S::Token(id) | S::Workload(id) | S::Extension(id)) => id.clone(),
        Some(S::Link(grant)) => format!("link {grant}"),
        Some(S::External(x)) => format!("{} {}", x.provider, super::identity::named(Some(x))),
        Some(S::System(_)) => "system".into(),
        Some(S::Anonymous(_)) => "anonymous".into(),
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

/// What `get effects` names: one of a workload, plan, node, or project, or else an
/// organization, the caller's own top one when nothing is named.
async fn effects_of(c: &Connection, e: &Effects) -> Result<pb::ListEffectsRequest, CliError> {
    let mut request = pb::ListEffectsRequest {
        plan: e.plan.clone(),
        node: e.node.clone(),
        action_prefix: e.action.clone().unwrap_or_default(),
        ..Default::default()
    };
    if let Some(w) = &e.workload {
        request.workload = Some(resolve::workload(c, w).await?);
    } else if let Some(p) = &e.project {
        request.project = Some(resolve::project(c, p).await?);
    } else if e.plan.is_none() && e.node.is_none() {
        request.organization = Some(resolve::organization(c, e.organization.as_deref()).await?);
    }
    Ok(request)
}

/// The effects `e` names, following page tokens until `e.limit` of them were read.
async fn effects(c: &Connection, e: &Effects) -> Result<Answer, CliError> {
    let mut request = effects_of(c, e).await?;
    let client = pb::EffectServiceClient::new(c.http(), c.config());
    let mut list = Vec::new();
    while list.len() < e.limit {
        let size = u32::try_from(e.limit - list.len()).unwrap_or(u32::MAX).min(1000);
        let token = std::mem::take(&mut request.page).into_option().unwrap_or_default();
        request.page = pb::PageRequest { page_size: size, ..token }.into();
        let page = client.list_effects(request.clone()).await?.into_owned();
        list.extend(page.effects);
        let next = page.page.into_option().unwrap_or_default().next_page_token;
        if next.is_empty() {
            break;
        }
        request.page = pb::PageRequest { page_token: next, ..Default::default() }.into();
    }
    list.truncate(e.limit);
    let header = ["AT", "ACTION", "OUTCOME", "ABOUT", "ACTOR", "GRANT", "RELAYED BY"];
    let human = rows(&header, &list, |e| {
        let outcome = short(e.outcome, "EFFECT_OUTCOME_");
        let about = e.object.as_option().map_or_else(|| "-".into(), |o| o.id.clone());
        let grant = e.grant.clone().unwrap_or_else(|| "-".into());
        let actor = subject(e.actor.as_option());
        let relay = subject(e.relay.as_option());
        vec![time(e.at.as_option()), e.action.clone(), outcome, about, actor, grant, relay]
    });
    Ok(Answer { json: json(&list), human })
}

/// The nodes of an organization.
async fn nodes(c: &Connection, organization: Option<&str>) -> Result<Answer, CliError> {
    let org = resolve::organization(c, organization).await?;
    let request = pb::ListNodesRequest { organization: org, ..Default::default() };
    let client = pb::NodeServiceClient::new(c.http(), c.config());
    let list = client.list_nodes(request).await?.into_owned().nodes;
    let human = rows(&["ID", "NAME", "STATUS", "OVERLAY", "AGENT", "LAST SEEN"], &list, |n| {
        let status = short(n.status, "NODE_STATUS_");
        vec![
            n.id.clone(),
            n.name.clone(),
            status,
            short(n.overlay, "OVERLAY_"),
            n.agent_version.clone(),
            time(n.last_seen.as_option()),
        ]
    });
    Ok(Answer { json: json(&list), human })
}

/// Every volume of `project`, or of one workload in it, page by page.
async fn volumes(
    c: &Connection,
    project: &str,
    workload: Option<&String>,
    deleted: bool,
) -> Result<Answer, CliError> {
    let project = resolve::project(c, project).await?;
    let workload = match workload {
        Some(w) => Some(resolve::workload(c, w).await?),
        None => None,
    };
    let client = pb::VolumeServiceClient::new(c.http(), c.config());
    let (mut list, mut token) = (Vec::new(), String::new());
    loop {
        let request = pb::ListVolumesRequest {
            project: project.clone(),
            workload: workload.clone(),
            include_deleted: deleted,
            page: pb::PageRequest { page_token: token, ..Default::default() }.into(),
            ..Default::default()
        };
        let listed = client.list_volumes(request).await?.into_owned();
        list.extend(listed.volumes);
        token = listed.page.into_option().map(|p| p.next_page_token).unwrap_or_default();
        if token.is_empty() {
            break;
        }
    }
    let header = ["ID", "NAME", "KIND", "CLASS", "SIZE", "NODE", "ATTACHED TO", "DELETED"];
    // A size nothing enforces is not shown (Rule K-3).
    let human = rows(&header, &list, |v| {
        let size = if v.quota_enforced { v.size_bytes.to_string() } else { "-".into() };
        let deleted = v.deleted_at.as_option().map_or("-".into(), |t| time(Some(t)));
        vec![
            v.id.clone(),
            v.name.clone(),
            short(v.kind, "VOLUME_KIND_"),
            v.storage_class.clone(),
            size,
            v.node.clone().unwrap_or_else(|| "-".into()),
            v.attached_to.clone().unwrap_or_else(|| "-".into()),
            deleted,
        ]
    });
    Ok(Answer { json: json(&list), human })
}

/// Answers `korpis get`.
pub async fn get(c: &Connection, what: &Get) -> Result<Answer, CliError> {
    match what {
        Get::Organizations { parent } => {
            let list = match parent {
                Some(p) => resolve::beneath(c, &resolve::organization(c, Some(p)).await?).await?,
                None => resolve::organizations(c).await?,
            };
            let human = rows(&["ID", "NAME", "PARENT", "STATE"], &list, |o| {
                let parent = o.parent.clone().unwrap_or_else(|| "-".into());
                let state = short(o.status, "ORGANIZATION_STATUS_");
                vec![o.id.clone(), o.name.clone(), parent, state]
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
        Get::Volumes { project, workload, deleted } => {
            volumes(c, project, workload.as_ref(), *deleted).await
        }
        Get::Nodes { organization } => nodes(c, organization.as_deref()).await,
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
        Get::Effects(e) => effects(c, e).await,
        Get::Usage(u) => super::usage::usage(c, u).await,
        Get::Periods(p) => super::usage::periods(c, p).await,
    }
}

/// Why nothing holds a workload, as the scheduler explained it to the caller.
/// One line per endpoint: its name, its exposure, and where a client reaches it, if anywhere.
fn endpoints(out: &mut String, s: &pb::WorkloadStatus) {
    for e in &s.endpoints {
        let exposure = short(e.exposure, "EXPOSURE_");
        let on_node = matches!(
            e.exposure.as_known(),
            Some(pb::Exposure::EXPOSURE_NODE_PORT | pb::Exposure::EXPOSURE_NODE_BOUND)
        );
        let at = e.address.as_option().map_or_else(
            || if on_node { "no free port on its node".to_owned() } else { "-".to_owned() },
            |a| {
                let at: Vec<String> = a.ips.iter().map(|ip| format!("{ip}:{}", a.port)).collect();
                if at.is_empty() { format!("port {}", a.port) } else { at.join(", ") }
            },
        );
        let _ = writeln!(out, "endpoint:  {} {exposure} {at}", e.name);
    }
}

fn unplaced(out: &mut String, s: &pb::WorkloadStatus) {
    let Some(u) = s.unplaced.as_option() else { return };
    let _ = writeln!(out, "unplaced since {}:", time(u.since.as_option()));
    for f in u.explanation.as_option().map(|e| e.filtered.as_slice()).unwrap_or_default() {
        let _ = writeln!(out, "  {}  {}: {}", f.node, f.check, f.detail);
    }
}

/// The quota-bearing figures of a declaration, by name.
fn figures(b: Option<&pb::IntentBody>) -> [(&'static str, u64); 6] {
    let r = b.and_then(|b| b.resources.as_option()).cloned().unwrap_or_default();
    let (cpu, memory, disk) = (
        r.cpu.into_option().unwrap_or_default(),
        r.memory.into_option().unwrap_or_default(),
        r.disk.into_option().unwrap_or_default(),
    );
    // Dedicated cores are held by their cpuset, not a quota: they are shown as cores, never as
    // a limit in millicores no kernel holds (Rule K-3, §5.2 of docs/05-scheduling.md).
    let dedicated =
        cpu.isolation.as_known() == Some(pb::CpuIsolation::CPU_ISOLATION_DEDICATED_CORES);
    let limit = if dedicated {
        ("dedicated cores", cpu.limit_millis / 1000)
    } else {
        ("cpu limit (millicores)", cpu.limit_millis)
    };
    [
        ("cpu reservation (millicores)", cpu.reservation_millis),
        limit,
        ("memory reservation (bytes)", memory.reservation),
        ("memory limit (bytes)", memory.limit),
        ("disk reservation (bytes)", disk.reservation),
        ("disk limit (bytes)", disk.limit),
    ]
}

/// A member changed outside its set: who changed it, and how far quota moved from what the
/// set's files say (finding 17 of docs/23-walkthroughs.md).
async fn drift(c: &Connection, out: &mut String, s: &pb::WorkloadStatus) -> Result<(), CliError> {
    let Some(d) = s.drift.as_option() else { return Ok(()) };
    let _ = writeln!(
        out,
        "drift:     changed outside set {} by {} at {} (effect {})",
        d.set,
        subject(d.declared_by.as_option()),
        time(d.declared_at.as_option()),
        d.effect.as_deref().unwrap_or("unknown"),
    );
    let Some(said) = &d.set_intent else { return Ok(()) };
    let workload = s.workload.as_option().map(|w| w.id.clone()).unwrap_or_default();
    let request = pb::ListIntentsRequest { workload, ..Default::default() };
    let client = pb::WorkloadServiceClient::new(c.http(), c.config());
    let intents = client.list_intents(request).await?.into_owned().intents;
    let files = intents.iter().find(|i| i.id == *said).and_then(|i| i.body.as_option());
    let now = s.head.as_option().and_then(|h| h.body.as_option());
    for ((what, set_says), (_, holds)) in figures(files).into_iter().zip(figures(now)) {
        if set_says != holds {
            let _ = writeln!(out, "           {what}: the set's files say {set_says}, now {holds}");
        }
    }
    Ok(())
}

/// Each dependency: what it reaches and how, and what it names and does not reach, with why
/// (§5 of docs/07-networking.md).
fn dependencies(out: &mut String, s: &pb::WorkloadStatus) {
    for d in &s.dependencies {
        let required = if d.required { ", required" } else { "" };
        let _ = writeln!(out, "depends:   {}{required}", d.name);
        for t in &d.targets {
            let reach = short(t.reach, "DEPENDENCY_REACH_").replace('_', " ");
            let _ = writeln!(out, "           {} {}: {reach}", t.workload, t.endpoint);
        }
        for l in &d.left_out {
            let _ = writeln!(out, "           {} left out: {}", l.workload, l.reason);
        }
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
    if s.held_by_suspension {
        out.push_str("held:      stopped while its organization, or one above it, is suspended\n");
    }
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
    endpoints(&mut out, &s);
    dependencies(&mut out, &s);
    unplaced(&mut out, &s);
    drift(c, &mut out, &s).await?;
    Ok(Answer { json: json(&s), human: out })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn dedicated_cores_are_shown_as_cores_and_never_as_a_limit_no_quota_holds() {
        let body = |isolation: pb::CpuIsolation| pb::IntentBody {
            resources: pb::ResourceSpec {
                cpu: pb::CpuSpec {
                    reservation_millis: 2000,
                    limit_millis: 2000,
                    isolation: isolation.into(),
                    ..Default::default()
                }
                .into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        };
        let held = figures(Some(&body(pb::CpuIsolation::CPU_ISOLATION_DEDICATED_CORES)));
        assert_eq!(held[1], ("dedicated cores", 2));
        let shared = figures(Some(&body(pb::CpuIsolation::CPU_ISOLATION_SHARED)));
        assert_eq!(shared[1], ("cpu limit (millicores)", 2000));
    }
}
