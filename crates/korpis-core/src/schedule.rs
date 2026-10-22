// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placement (§2 of docs/05-scheduling.md): filter the nodes that can run a workload, score the
//! survivors, choose, and record why. Pure: the caller supplies the nodes, what is already
//! allocated on them, and the workload's placed peers, all read in one transaction under the
//! placement lock, so the answer is consistent with what the bind will write.

pub mod churn;
mod filter;
pub mod policy;
mod score;
mod topology;
pub mod view;

pub use policy::{PlacementPolicy, Signal, SpreadConstraint, SpreadScope, Strategy};

use crate::node::Node;
use crate::tenancy::quota::{Bounds, Dimension, Held, QuotaEntry, Totals};
use crate::workload::IntentBody;
use std::collections::BTreeMap;
use uuid::Uuid;

/// The dimensions placement guarantees: a reservation in one of these is capacity a node sets
/// aside. The others are quota's (§5 of docs/05-scheduling.md) or limits only.
pub const RESERVED: [Dimension; 5] = [
    Dimension::Cpu,
    Dimension::Memory,
    Dimension::Disk,
    Dimension::Bandwidth,
    Dimension::NodePorts,
];

/// What a placement of `body` reserves on its node.
#[must_use]
pub fn reservation(body: &IntentBody) -> Bounds {
    let held = Held::of(body);
    RESERVED
        .into_iter()
        .map(|d| (d, held.reservations.get(&d).copied().unwrap_or(0)))
        .filter(|(_, n)| *n > 0)
        .collect()
}

/// The workload being placed.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// Its identity.
    pub workload: Uuid,
    /// Its project.
    pub project: Uuid,
    /// Its labels, which spread constraints and other workloads' affinity rules select on.
    pub labels: &'a BTreeMap<String, String>,
    /// The organizations whose nodes may serve it: its own and every ancestor (§3.4 of
    /// docs/01-model.md).
    pub owners: &'a [Uuid],
    /// The approved intent being placed.
    pub body: &'a IntentBody,
    /// Where it is placed now, and what that placement reserves, when it is re-placed for a
    /// new target.
    pub current: Option<Current<'a>>,
    /// Whether an edge tier serves it, which decides what a declared `stable` endpoint
    /// resolves to and so what it needs of a node (§3 of docs/07-networking.md).
    pub edge_tier: bool,
    /// Each existing volume the intent attaches, with the node holding it: a node-local volume
    /// is reachable from its node alone (§7 of docs/06-storage.md).
    pub volumes: &'a [(Uuid, Uuid)],
    /// The nodes whose driver refused this intent, each with its reason: asked on the
    /// shortlist, or after a bind (§2.1 of docs/05-scheduling.md).
    pub refused: &'a BTreeMap<Uuid, String>,
    /// Each node that reported its content, with whether it holds this intent's recipe (§2.2
    /// of docs/05-scheduling.md); content locality is scored for those alone.
    pub content: &'a BTreeMap<Uuid, bool>,
    /// Each node with recent loss events, with what they weigh now ([`churn::loss_weight`]); a
    /// node absent lost nothing it remembers.
    pub churn: &'a BTreeMap<Uuid, f64>,
    /// What its dependencies reach, resolved (§5 of docs/07-networking.md).
    pub dependencies: &'a [Needed],
    /// The quota entries counting it that count where it runs (§5.1 of
    /// docs/05-scheduling.md).
    pub quota: &'a [Counting<'a>],
}

/// A quota entry that counts where workloads run, as placement checks it: the entry, what
/// everything else it counts holds, and what it already counts of this workload, if anything.
#[derive(Debug, Clone)]
pub struct Counting<'a> {
    /// The entry.
    pub entry: &'a QuotaEntry,
    /// What it counts beside this workload.
    pub rest: Totals,
    /// What it counts of this workload now, placed on a node it counts.
    pub counted: Option<&'a Held>,
    /// What it promised the organizations beneath its owner, by their entries of its shape
    /// (Finding 139 of docs/23-walkthroughs.md); none for a project's entry.
    pub promises: Vec<crate::tenancy::quota::Promise>,
}

/// A dependency as placement reads it: its name, whether it is required, and the workloads it
/// resolved to, whose nodes the peers say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needed {
    /// Its name.
    pub name: String,
    /// Whether the dependent may not start without it.
    pub required: bool,
    /// The workloads it reaches.
    pub targets: Vec<Uuid>,
}

/// A workload's existing placement.
#[derive(Debug, Clone, Copy)]
pub struct Current<'a> {
    /// The node.
    pub node: Uuid,
    /// What it reserves there.
    pub reserved: &'a Bounds,
}

/// A node, with what its placements reserve.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a> {
    /// The node.
    pub node: &'a Node,
    /// Summed over its placements.
    pub allocated: &'a Bounds,
}

/// A placed workload of the same organization: what affinity and spread count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// The workload.
    pub workload: Uuid,
    /// Its project.
    pub project: Uuid,
    /// Its node.
    pub node: Uuid,
    /// Its labels.
    pub labels: BTreeMap<String, String>,
}

/// A filter check (§2.1 of docs/05-scheduling.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Check {
    /// The node serves the workload's organization.
    Owner,
    /// The node is ready.
    Status,
    /// A healthy driver of the requested name.
    Driver,
    /// The driver implements the requested tier.
    Tier,
    /// The driver offers the requested console.
    Console,
    /// The driver holds every limit the intent sets (Rule K-3).
    Enforcement,
    /// The node can be reached the way the endpoints need.
    Reachability,
    /// The intent's node selector.
    Selector,
    /// Every taint is tolerated.
    Taint,
    /// Unallocated capacity for every reservation.
    Capacity,
    /// Every named storage class is offered.
    StorageClass,
    /// Every existing volume attached is on the node.
    Volume,
    /// Every required affinity and anti-affinity rule.
    Affinity,
    /// Every dependency's targets are reachable from the node (§5 of docs/07-networking.md).
    Dependency,
    /// Every quota entry counting what runs on the node admits it (§5.1 of
    /// docs/05-scheduling.md).
    Quota,
    /// The node's driver accepted the intent when the shortlist asked it, or after it was bound
    /// there (§2.1 of docs/05-scheduling.md).
    Prepare,
}

impl Check {
    /// Every check, in the order they are evaluated.
    pub const ALL: [Self; 16] = [
        Self::Owner,
        Self::Status,
        Self::Driver,
        Self::Tier,
        Self::Console,
        Self::Enforcement,
        Self::Reachability,
        Self::Selector,
        Self::Taint,
        Self::Capacity,
        Self::StorageClass,
        Self::Volume,
        Self::Affinity,
        Self::Dependency,
        Self::Quota,
        Self::Prepare,
    ];

    /// The check a name names.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == s)
    }

    /// Its name in an explanation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Status => "status",
            Self::Driver => "driver",
            Self::Tier => "tier",
            Self::Console => "console",
            Self::Enforcement => "enforcement",
            Self::Reachability => "reachability",
            Self::Selector => "selector",
            Self::Taint => "taint",
            Self::Capacity => "capacity",
            Self::StorageClass => "storage_class",
            Self::Volume => "volume",
            Self::Affinity => "affinity",
            Self::Dependency => "dependency",
            Self::Quota => "quota",
            Self::Prepare => "prepare",
        }
    }
}

/// One node failing one check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// The node.
    pub node: Uuid,
    /// The check.
    pub check: Check,
    /// Why, for a human.
    pub detail: String,
}

/// One signal's value for one node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    /// The node.
    pub node: Uuid,
    /// The signal.
    pub signal: Signal,
    /// From 0 to 1, higher preferred.
    pub value: f64,
    /// Its weight in the policy.
    pub weight: f64,
}

/// The full derivation of one placement decision (§2.3 of docs/05-scheduling.md), stored
/// complete and rendered per viewer (§8 of docs/08-identity.md).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Explanation {
    /// Nodes considered.
    pub candidates: u32,
    /// Every failing check of every rejected node.
    pub filtered: Vec<Rejection>,
    /// Every evaluated signal of every surviving node.
    pub scored: Vec<Score>,
    /// The node chosen; `None` when no node survived the filter.
    pub chosen: Option<Uuid>,
    /// The next best.
    pub runner_up: Option<Uuid>,
    /// The policy, as evaluated.
    pub policy: PlacementPolicy,
}

/// The refusals of `c` that stand when an operator chooses it by hand (§2.3 of
/// docs/05-scheduling.md). A cordon and a taint record the operator's own decisions about the
/// node, so choosing it overrides them; every other check is a fact about whether the workload
/// can run there, or its tenant's own declaration, and holds.
#[must_use]
pub fn operator_refusals(
    req: &Request<'_>,
    c: &Candidate<'_>,
    peers: &[Peer],
    policy: &PlacementPolicy,
) -> Vec<Rejection> {
    let fleet: topology::Fleet<'_> = std::iter::once((c.node.id, c.node)).collect();
    let cordoned = c.node.admin == crate::node::NodeAdmin::Cordoned
        && c.node.connectivity == crate::node::Connectivity::Connected;
    filter::rejections(req, c, &fleet, peers, policy)
        .into_iter()
        .filter(|(check, _)| match check {
            Check::Taint => false,
            Check::Status => !cordoned,
            _ => true,
        })
        .map(|(check, detail)| Rejection { node: c.node.id, check, detail })
        .collect()
}

/// Filters, scores, and chooses. Ties go to the lower node identifier, so the same inputs
/// always choose the same node.
#[must_use]
pub fn place(
    req: &Request<'_>,
    nodes: &[Candidate<'_>],
    peers: &[Peer],
    policy: &PlacementPolicy,
) -> Explanation {
    ranked(req, nodes, peers, policy).0
}

/// As [`place`], with every node that survived the filter, best first: the shortlist a
/// `FeasibilityQuery` asks is its head (§2.1 of docs/05-scheduling.md).
#[must_use]
pub fn ranked(
    req: &Request<'_>,
    nodes: &[Candidate<'_>],
    peers: &[Peer],
    policy: &PlacementPolicy,
) -> (Explanation, Vec<Uuid>) {
    let fleet: topology::Fleet<'_> = nodes.iter().map(|c| (c.node.id, c.node)).collect();
    let mut filtered = Vec::new();
    let mut survivors = Vec::new();
    for c in nodes {
        let rejected = filter::rejections(req, c, &fleet, peers, policy);
        if rejected.is_empty() {
            survivors.push(*c);
        }
        filtered.extend(rejected.into_iter().map(|(check, detail)| Rejection {
            node: c.node.id,
            check,
            detail,
        }));
    }
    let (scored, ranking) = score::rank(req, &survivors, &fleet, peers, policy);
    let explanation = Explanation {
        candidates: u32::try_from(nodes.len()).unwrap_or(u32::MAX),
        filtered,
        scored,
        chosen: ranking.first().copied(),
        runner_up: ranking.get(1).copied(),
        policy: policy.clone(),
    };
    (explanation, ranking)
}

#[cfg(test)]
mod tests;
