// SPDX-License-Identifier: AGPL-3.0-or-later

//! The ingress on one machine (§3.2 of docs/07-networking.md): a workload with an `ingress`
//! endpoint is placed only on a node serving the ingress, which holds ports 80 and 443.

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

#[test]
fn an_ingress_endpoint_is_placed_only_where_the_node_serves_the_ingress() {
    let mut without = node(1, &[]);
    without.advertised = Some(serving(false));
    let mut with = node(2, &[]);
    with.advertised = Some(serving(true));
    let mut case = Case::new(vec![without, node(3, &[]), with]);
    case.body.endpoints = vec![EndpointSpec {
        name: "web".into(),
        protocol: Protocol::Http,
        container_port: 8080,
        exposure: Exposure::Ingress,
        domains: vec!["shop.example.com".into()],
        tls: Some(TlsSpec::Acme { clients: None }),
        policy: NetworkPolicy::default(),
        accounting: false,
        shaping: ShapingSpec::default(),
    }];
    let e = case.run();
    assert_eq!(checks(&e, 1), [Check::Reachability]);
    assert_eq!(checks(&e, 3), [Check::Reachability], "a node that never said");
    assert!(e.filtered[0].detail.contains("80 and 443"), "{}", e.filtered[0].detail);
    assert_eq!(e.chosen, Some(Uuid::from_u128(2)));
}
