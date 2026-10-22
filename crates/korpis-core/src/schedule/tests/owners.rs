// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placement policy per organization (§2.2 of docs/05-scheduling.md, question 8 of §10): a node
//! is scored by the strategy and weights of its owner's policy, the nearest owner's nodes are
//! ranked first, and a workload's spread constraints are its own wherever it runs.

use super::{GIB, ORG, PROJECT, body, node, req};
use crate::node::Node;
use crate::schedule::{
    Candidate, Peer, PlacementPolicy, Policies, Request, Signal, SpreadConstraint, SpreadScope,
    Strategy, place,
};
use crate::tenancy::quota::{Bounds, Dimension};
use std::collections::BTreeMap;
use uuid::Uuid;

/// The host, the root organization above the customer `ORG`.
const HOST: Uuid = Uuid::from_u128(1);

fn owned(n: u128, owner: Uuid, labels: &[(&str, &str)]) -> Node {
    Node { organization: owner, ..node(n, labels) }
}

fn holding(memory: u64) -> Bounds {
    Bounds::from([(Dimension::Memory, memory)])
}

fn strategy(s: Strategy) -> PlacementPolicy {
    PlacementPolicy { strategy: s, ..PlacementPolicy::default() }
}

/// The customer's own policy and the host's, each scoring the nodes its organization owns.
fn policies(own: PlacementPolicy, host: PlacementPolicy) -> Policies {
    Policies { workload: own.clone(), owners: BTreeMap::from([(ORG, own), (HOST, host)]) }
}

/// A request of the customer's, below the host.
fn below<'a>(
    labels: &'a BTreeMap<String, String>,
    b: &'a crate::workload::IntentBody,
) -> Request<'a> {
    Request { owners: &[ORG, HOST], ..req(labels, b) }
}

#[test]
fn the_nearest_owner_s_node_is_chosen_while_one_can_run_the_workload() {
    let (own, host) = (owned(1, ORG, &[]), owned(2, HOST, &[]));
    let (half, empty) = (holding(4 * GIB), Bounds::new());
    let nodes =
        [Candidate { node: &own, allocated: &half }, Candidate { node: &host, allocated: &empty }];
    let (labels, b) = (BTreeMap::new(), body(GIB));
    let both = policies(strategy(Strategy::Distributed), strategy(Strategy::Distributed));
    let e = place(&below(&labels, &b), &nodes, &[], &both);
    assert_eq!(e.chosen, Some(own.id), "its own node, though the host's is emptier");
    assert_eq!(e.runner_up, Some(host.id), "the host's stays behind it on the shortlist");
    assert_eq!(e.owners.len(), 2, "each owner's policy is recorded");

    let full = holding(8 * GIB);
    let nodes = [Candidate { node: &own, allocated: &full }, nodes[1]];
    let e = place(&below(&labels, &b), &nodes, &[], &both);
    assert_eq!(e.chosen, Some(host.id), "the host's, once its own has no room");
}

#[test]
fn an_owner_s_nodes_are_scored_by_its_own_strategy() {
    let (fuller, emptier) = (owned(1, HOST, &[]), owned(2, HOST, &[]));
    let (some, none) = (holding(2 * GIB), Bounds::new());
    let nodes = [
        Candidate { node: &fuller, allocated: &some },
        Candidate { node: &emptier, allocated: &none },
    ];
    let (labels, b) = (BTreeMap::new(), body(GIB));
    let distributing = strategy(Strategy::Distributed);
    let packing_host = policies(distributing.clone(), strategy(Strategy::Packed));
    let e = place(&below(&labels, &b), &nodes, &[], &packing_host);
    assert_eq!(e.chosen, Some(fuller.id), "the host packs its fleet, whatever the tenant prefers");
    let spreading_host = policies(strategy(Strategy::Packed), distributing);
    let e = place(&below(&labels, &b), &nodes, &[], &spreading_host);
    assert_eq!(e.chosen, Some(emptier.id), "and spreads it when it says so");
}

#[test]
fn a_workload_s_spread_is_its_own_on_whoever_s_nodes() {
    let (a, b_node) = (owned(1, HOST, &[("rack", "a")]), owned(2, HOST, &[("rack", "b")]));
    let none = Bounds::new();
    let nodes =
        [Candidate { node: &a, allocated: &none }, Candidate { node: &b_node, allocated: &none }];
    let peer = Peer {
        workload: Uuid::from_u128(600),
        project: PROJECT,
        node: a.id,
        labels: BTreeMap::new(),
    };
    let spread =
        SpreadConstraint { topology_key: "rack".into(), max_skew: 0, scope: SpreadScope::Project };
    let own = PlacementPolicy { spread: vec![spread], ..PlacementPolicy::default() };
    let mut host = strategy(Strategy::Packed);
    host.weights.insert(Signal::FailureDomain, 10.0);
    let (labels, b) = (BTreeMap::new(), body(GIB));
    let e = place(&below(&labels, &b), &nodes, std::slice::from_ref(&peer), &policies(own, host));
    assert_eq!(e.chosen, Some(b_node.id), "the tenant's constraint, weighed by the host's weight");
    let unspread = policies(PlacementPolicy::default(), strategy(Strategy::Packed));
    let e = place(&below(&labels, &b), &nodes, &[peer], &unspread);
    assert_eq!(e.chosen, Some(a.id), "with no spread of the tenant's, the packing tie goes low");
}
