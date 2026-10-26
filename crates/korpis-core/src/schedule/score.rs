// SPDX-License-Identifier: AGPL-3.0-or-later

//! Scoring (§2.2 of docs/05-scheduling.md): of the nodes that can run this, which is best. A
//! signal is scored only where its information exists: content locality only for a node that
//! reported its content, rather than a guess for one that did not. Churn is scored for every
//! node, since the control plane records every loss itself and a node with none lost nothing.

use super::topology::{Fleet, counts, domain, matching};
use super::{
    Candidate, Peer, PlacementPolicy, Policies, RESERVED, Request, Score, Signal, SpreadScope,
    Strategy, reservation,
};
use crate::labels::LabelSelector;
use crate::node::Node;
use crate::value::uint_to_f64;
use crate::workload::spec::placement::AffinityRule;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use uuid::Uuid;

/// The node's utilization per reserved dimension it declares, after this placement.
fn utilization(req: &Request<'_>, c: &Candidate<'_>, strategy: Strategy) -> f64 {
    let need = reservation(req.body, req.edge_tier);
    let own = req.current.filter(|cur| cur.node == c.node.id).map(|cur| cur.reserved);
    let fractions: Vec<f64> = RESERVED
        .into_iter()
        .filter_map(|d| {
            let total = c.node.allocatable.get(&d).copied().filter(|n| *n > 0)?;
            let mine = own.and_then(|o| o.get(&d)).copied().unwrap_or(0);
            let used = c.allocated.get(&d).copied().unwrap_or(0).saturating_sub(mine);
            let after = used.saturating_add(need.get(&d).copied().unwrap_or(0));
            Some((uint_to_f64(after) / uint_to_f64(total)).min(1.0))
        })
        .collect();
    if fractions.is_empty() {
        return 0.0;
    }
    let mean =
        fractions.iter().sum::<f64>() / uint_to_f64(fractions.len().try_into().unwrap_or(u64::MAX));
    let max = fractions.iter().copied().fold(0.0, f64::max);
    match strategy {
        Strategy::Packed => mean,
        Strategy::Distributed => 1.0 - mean,
        Strategy::Balanced => 1.0 - max,
    }
}

fn preferred_rule(
    req: &Request<'_>,
    node: &Node,
    fleet: &Fleet<'_>,
    peers: &[Peer],
    rule: &AffinityRule,
) -> f64 {
    let Some(here) = domain(node, &rule.topology_key) else { return 0.0 };
    let matched = matching(fleet, peers, &rule.topology_key, &rule.workloads, req.workload);
    let together = matched.iter().any(|(_, d)| d.as_deref() == Some(here.as_str()));
    // Affinity wants company in the domain, anti-affinity wants none.
    let kept = if rule.anti { !together } else { together };
    if kept { 1.0 } else { 0.0 }
}

/// Whether the constraint counts a workload with these labels in this project.
fn counted(
    req: &Request<'_>,
    scope: &SpreadScope,
    labels: &BTreeMap<String, String>,
    project: Uuid,
) -> bool {
    match scope {
        SpreadScope::Project => project == req.project,
        SpreadScope::Organization => true,
        SpreadScope::Selector(sel) => sel.matches(labels),
    }
}

/// Whether placing on `node` keeps every spread constraint that counts this workload within
/// its skew, over the domains of the nodes that can run it.
fn spread(
    req: &Request<'_>,
    node: &Node,
    eligible: &[&Node],
    fleet: &Fleet<'_>,
    peers: &[Peer],
    policy: &PlacementPolicy,
) -> Vec<f64> {
    policy
        .spread
        .iter()
        .filter(|s| counted(req, &s.scope, req.labels, req.project))
        .map(|s| {
            let Some(here) = domain(node, &s.topology_key) else { return 0.0 };
            let mine: Vec<Peer> = peers
                .iter()
                .filter(|p| counted(req, &s.scope, &p.labels, p.project))
                .cloned()
                .collect();
            let all = LabelSelector::default();
            let matched = matching(fleet, &mine, &s.topology_key, &all, req.workload);
            let mut per = counts(eligible, &matched, &s.topology_key);
            *per.entry(here).or_insert(0) += 1;
            let max = per.values().copied().max().unwrap_or(0);
            let min = per.values().copied().min().unwrap_or(0);
            if max - min <= u64::from(s.max_skew) { 1.0 } else { 0.0 }
        })
        .collect()
}

/// The mean of the failure-domain preferences that apply; `None` when none does.
fn failure_domain(
    req: &Request<'_>,
    node: &Node,
    eligible: &[&Node],
    fleet: &Fleet<'_>,
    peers: &[Peer],
    policy: &PlacementPolicy,
) -> Option<f64> {
    let mut parts = spread(req, node, eligible, fleet, peers, policy);
    let rules = req.body.placement.affinity.iter().chain(&policy.affinity);
    parts.extend(rules.filter(|r| !r.required).map(|r| preferred_rule(req, node, fleet, peers, r)));
    let region = &req.body.placement.preferred_region;
    if !region.is_empty() {
        let here = node.labels.as_map().get("region") == Some(region);
        parts.push(if here { 1.0 } else { 0.0 });
    }
    (!parts.is_empty()).then(|| {
        parts.iter().sum::<f64>() / uint_to_f64(parts.len().try_into().unwrap_or(u64::MAX))
    })
}

/// Every evaluated signal of every survivor, and the survivors best first: the nearest owner's
/// nodes before a farther one's, and within one owner the best scored by that owner's strategy
/// and weights, so two policies' scores are never compared (§2.2 of docs/05-scheduling.md). A
/// spread constraint and an affinity rule are the workload's, whoever owns the node.
pub(super) fn rank(
    req: &Request<'_>,
    survivors: &[Candidate<'_>],
    fleet: &Fleet<'_>,
    peers: &[Peer],
    policies: &Policies,
) -> (Vec<Score>, Vec<Uuid>) {
    let eligible: Vec<&Node> = survivors.iter().map(|c| c.node).collect();
    let mut scored = Vec::new();
    let mut totals = Vec::new();
    for c in survivors {
        let id = c.node.id;
        let owner = c.node.organization;
        let nearness = req.owners.iter().position(|o| *o == owner).unwrap_or(usize::MAX);
        let policy = policies.of_owner(owner);
        let mut values = vec![(Signal::Utilization, utilization(req, c, policy.strategy))];
        if let Some(cur) = req.current {
            // Phase 0 volumes live on the node of the placement that created them.
            values.push((Signal::VolumeLocality, if cur.node == id { 1.0 } else { 0.0 }));
        }
        // A node's devices are spared for workloads that ask for them (§6.3 of
        // docs/05-scheduling.md): what its driver could give is not what the node has.
        let spared = req.body.devices.is_empty() && !c.node.devices.is_empty();
        values.push((Signal::DeviceFit, if spared { 0.0 } else { 1.0 }));
        if let Some(v) = failure_domain(req, c.node, &eligible, fleet, peers, &policies.workload) {
            values.push((Signal::FailureDomain, v));
        }
        if let Some(held) = req.content.get(&id) {
            values.push((Signal::ContentLocality, if *held { 1.0 } else { 0.0 }));
        }
        let lost = req.churn.get(&id).copied().unwrap_or(0.0);
        values.push((Signal::Churn, super::churn::churn_score(lost)));
        let mut total = 0.0;
        for (signal, value) in values {
            let weight = policy.weight(signal);
            total += weight * value;
            scored.push(Score { node: id, signal, value, weight });
        }
        totals.push((nearness, id, total));
    }
    totals.sort_by(|a, b| {
        a.0.cmp(&b.0).then(b.2.partial_cmp(&a.2).unwrap_or(Ordering::Equal)).then(a.1.cmp(&b.1))
    });
    (scored, totals.into_iter().map(|(_, id, _)| id).collect())
}
