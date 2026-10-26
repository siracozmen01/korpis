// SPDX-License-Identifier: AGPL-3.0-or-later

//! Edges (§3.1 of docs/07-networking.md): a workload whose `stable` endpoint an edge group
//! serves is placed only on a node that reaches every member through the mesh, and holds no
//! port of it.

use super::{Case, checks, node};
use crate::schedule::{Check, reservation};
use crate::tenancy::quota::Dimension;
use crate::workload::spec::endpoints::NetworkPolicy;
use crate::workload::spec::endpoints::{EndpointSpec, Exposure, Protocol, ShapingSpec};
use uuid::Uuid;

fn stable() -> EndpointSpec {
    EndpointSpec {
        name: "game".into(),
        protocol: Protocol::Udp,
        container_port: 25_565,
        exposure: Exposure::Stable,
        domains: Vec::new(),
        tls: None,
        policy: NetworkPolicy::default(),
        accounting: false,
        shaping: ShapingSpec::default(),
    }
}

#[test]
fn a_stable_endpoint_an_edge_serves_needs_every_member_reached_and_no_port_of_the_node() {
    let (one, two, e1, e2) =
        (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(5), Uuid::from_u128(6));
    let mut case = Case::new(vec![node(1, &[]), node(2, &[])]);
    case.body.endpoints = vec![stable()];
    case.edge_members = vec![e1, e2];
    case.mesh = [(one, e1), (one, e2), (two, e1)].into_iter().collect();
    let e = case.run();
    assert!(checks(&e, 1).is_empty(), "{:?}", e.filtered);
    assert_eq!(checks(&e, 2), [Check::Edge], "it does not reach the second edge");
    assert_eq!(e.chosen, Some(one));

    case.edge_members = vec![two, e1];
    assert!(checks(&case.run(), 2).is_empty(), "a member reaches itself");

    assert_eq!(reservation(&case.body, false).get(&Dimension::NodePorts), Some(&1));
    assert_eq!(reservation(&case.body, true).get(&Dimension::NodePorts), None, "the edge's");
    case.edge_members.clear();
    let bound = checks(&case.run(), 2);
    assert!(!bound.contains(&Check::Edge), "node_bound asks nothing of the mesh: {bound:?}");
}
