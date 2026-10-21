// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bandwidth, held by the node's network (§7 of docs/07-networking.md): a workload that sets an
//! endpoint's own shaping or accounting, or a reservation of the node's uplinks, is placed only
//! on a node whose link-joining driver declares it enforces each, and its reservation only
//! within the bandwidth that node offers.

use super::{Case, checks, driver, node};
use crate::schedule::Check;
use crate::tenancy::quota::{Bounds, Dimension};
use crate::workload::spec::endpoints::{
    EndpointSpec, Exposure, NetworkPolicy, Protocol, ShapingSpec,
};
use uuid::Uuid;

const MBIT: u64 = 1_000_000;

fn game(accounting: bool, egress: u64) -> EndpointSpec {
    EndpointSpec {
        name: "game".into(),
        protocol: Protocol::Udp,
        container_port: 25565,
        exposure: Exposure::NodePort,
        domains: Vec::new(),
        tls: None,
        policy: NetworkPolicy::default(),
        accounting,
        shaping: ShapingSpec { egress_bits_per_sec: egress, ingress_bits_per_sec: 0 },
    }
}

/// A node with ports of its own to give a `node_port` endpoint.
fn ported(n: u128) -> crate::node::Node {
    let mut node = node(n, &[]);
    node.allocatable.insert(Dimension::NodePorts, 100);
    node
}

#[test]
fn an_endpoints_own_shaping_and_accounting_are_placed_only_where_the_network_holds_them() {
    let mut case = Case::new(vec![ported(1)]);
    case.body.endpoints = vec![game(true, MBIT)];
    let e = case.run();
    let detail = &e.filtered[0].detail;
    assert!(
        detail.contains("endpoint_shaping") && detail.contains("endpoint_accounting"),
        "{detail}"
    );
    let mut counts = ported(2);
    counts.drivers = vec![driver(&["memory", "cpu", "endpoint_accounting"])];
    case.nodes.push(counts);
    assert_eq!(case.run().chosen, None, "counted, and not shaped");
    case.body.endpoints = vec![game(true, 0)];
    assert_eq!(case.run().chosen, Some(Uuid::from_u128(2)), "counted is all it asks");
}

#[test]
fn a_reservation_is_placed_within_the_bandwidth_a_node_with_a_shaped_uplink_offers() {
    let mut shaped = ported(2);
    shaped.drivers = vec![driver(&["memory", "cpu", "egress", "ingress", "endpoint_shaping"])];
    let offered = 950 * MBIT;
    shaped.allocatable.insert(Dimension::Bandwidth, offered);
    shaped.capacity.insert(Dimension::Bandwidth, 1_000 * MBIT);
    let mut case = Case::new(vec![ported(1), shaped]);
    case.body.resources.bandwidth.egress.reservation = 100 * MBIT;
    case.body.endpoints = vec![game(false, 10 * MBIT)];
    let e = case.run();
    assert_eq!(checks(&e, 1), [Check::Enforcement, Check::Capacity], "no uplink declared");
    assert_eq!(e.chosen, Some(Uuid::from_u128(2)));
    case.allocated.insert(Uuid::from_u128(2), Bounds::from([(Dimension::Bandwidth, 900 * MBIT)]));
    let e = case.run();
    assert_eq!(checks(&e, 2), [Check::Capacity], "never oversubscribed");
    assert_eq!(e.chosen, None);
}
