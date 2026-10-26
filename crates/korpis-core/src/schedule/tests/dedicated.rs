// SPDX-License-Identifier: AGPL-3.0-or-later

//! Dedicated addresses (§3.7 of docs/07-networking.md): a workload with a `dedicated_ip`
//! endpoint is placed only on a node it may hold its address on, and one without asks nothing.

use super::{Case, checks, node};
use crate::schedule::Check;
use crate::workload::spec::endpoints::NetworkPolicy;
use crate::workload::spec::endpoints::{EndpointSpec, Exposure, Protocol, ShapingSpec};
use uuid::Uuid;

fn endpoint(exposure: Exposure) -> EndpointSpec {
    EndpointSpec {
        name: "game".into(),
        protocol: Protocol::Udp,
        container_port: 27015,
        exposure,
        domains: Vec::new(),
        tls: None,
        policy: NetworkPolicy::default(),
        accounting: false,
        shaping: ShapingSpec::default(),
    }
}

#[test]
fn a_dedicated_address_is_held_only_on_a_node_a_pool_reaches() {
    let mut case = Case::new(vec![node(1, &[]), node(2, &[])]);
    case.body.endpoints = vec![endpoint(Exposure::DedicatedIp)];
    case.dedicated.insert(Uuid::from_u128(2));
    let e = case.run();
    assert_eq!(checks(&e, 1), [Check::DedicatedIp]);
    assert!(checks(&e, 2).is_empty());
    assert_eq!(e.chosen, Some(Uuid::from_u128(2)));
    case.dedicated.clear();
    assert_eq!(case.run().chosen, None, "no pool reaches either node");
    case.body.endpoints = vec![endpoint(Exposure::OverlayOnly)];
    assert!(checks(&case.run(), 1).is_empty(), "an endpoint not dedicated asks for no pool");
}
