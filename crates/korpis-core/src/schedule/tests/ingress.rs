// SPDX-License-Identifier: AGPL-3.0-or-later

//! The ingress on one machine (§3.2 of docs/07-networking.md): a workload with an `ingress`
//! endpoint is placed only on a node serving the ingress, which holds ports 80 and 443, unless
//! an edge group's members serve it (§3.1 there), whatever serves its `stable` endpoints.

use super::{Case, checks, node};
use crate::node::advertised::{Advertised, PortRange};
use crate::schedule::Check;
use crate::workload::spec::endpoints::{
    EndpointSpec, Exposure, NetworkPolicy, Protocol, ShapingSpec, TlsSpec,
};
use uuid::Uuid;

fn serving(ingress: bool) -> Advertised {
    let addresses = vec![std::net::Ipv4Addr::new(203, 0, 113, 10).into()];
    Advertised { addresses, ports: PortRange::DEFAULT, ingress }
}

fn endpoint(name: &str, exposure: Exposure) -> EndpointSpec {
    let ingress = exposure == Exposure::Ingress;
    EndpointSpec {
        name: name.into(),
        protocol: if ingress { Protocol::Http } else { Protocol::Udp },
        container_port: 8080,
        exposure,
        domains: if ingress { vec!["shop.example.com".into()] } else { Vec::new() },
        tls: ingress.then_some(TlsSpec::Acme { clients: None }),
        policy: NetworkPolicy::default(),
        accounting: false,
        shaping: ShapingSpec::default(),
    }
}

#[test]
fn an_ingress_endpoint_is_placed_only_where_the_node_serves_the_ingress() {
    let mut without = node(1, &[]);
    without.advertised = Some(serving(false));
    let mut with = node(2, &[]);
    with.advertised = Some(serving(true));
    let mut case = Case::new(vec![without, node(3, &[]), with]);
    case.body.endpoints = vec![endpoint("web", Exposure::Ingress)];
    let e = case.run();
    assert_eq!(checks(&e, 1), [Check::Reachability]);
    assert_eq!(checks(&e, 3), [Check::Reachability], "a node that never said");
    assert!(e.filtered[0].detail.contains("80 and 443"), "{}", e.filtered[0].detail);
    assert_eq!(e.chosen, Some(Uuid::from_u128(2)));
}

#[test]
fn an_edge_group_serving_the_stable_endpoints_alone_leaves_the_ingress_to_the_node() {
    let mut without = node(1, &[]);
    without.advertised = Some(serving(false));
    let mut case = Case::new(vec![without]);
    case.body.endpoints =
        vec![endpoint("game", Exposure::Stable), endpoint("web", Exposure::Ingress)];
    // A group serves its `stable` endpoint and not its ingress: the node must serve that
    // (Finding 197 of docs/23-walkthroughs.md).
    case.edge_members = vec![Uuid::from_u128(1)];
    assert_eq!(checks(&case.run(), 1), [Check::Reachability]);
    case.ingress_edge = true;
    assert_eq!(case.run().chosen, Some(Uuid::from_u128(1)), "the group's proxies serve it");
}
