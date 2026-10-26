// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;
use crate::workload::spec::endpoints::{
    EndpointSpec, Exposure, NetworkPolicy, PortRange, Protocol, ShapingSpec,
};

fn web() -> LabelSelector {
    LabelSelector { match_labels: [("role".into(), "web".into())].into(), ..Default::default() }
}

fn rule(action: NetworkAction, peer: Peer) -> NetworkRule {
    NetworkRule { action, peer, ports: Vec::<PortRange>::new(), protocol: None }
}

/// A body whose endpoint `pg` allows workloads `role=web`, then denies every IPv4 address, and
/// whose egress denies them.
fn body() -> IntentBody {
    let mut b = crate::workload::spec::fixture();
    let policy = NetworkPolicy {
        ingress: vec![
            rule(NetworkAction::Allow, Peer::Workloads(web())),
            rule(NetworkAction::Deny, Peer::Cidr("0.0.0.0/0".parse().unwrap())),
        ],
        ..NetworkPolicy::default()
    };
    b.endpoints = vec![EndpointSpec {
        name: "pg".into(),
        protocol: Protocol::Tcp,
        container_port: 5432,
        exposure: Exposure::OverlayOnly,
        domains: Vec::new(),
        tls: None,
        policy,
        accounting: false,
        shaping: ShapingSpec::default(),
    }];
    b.egress.rules = vec![rule(NetworkAction::Deny, Peer::Workloads(web()))];
    b
}

fn ip(a: &str) -> IpAddr {
    a.parse().unwrap()
}

#[test]
fn every_rule_naming_workloads_is_found_where_it_is() {
    let b = body();
    let found: Vec<_> = naming_workloads(&b).iter().map(|n| (n.place, n.allow)).collect();
    assert_eq!(found, [((Some("pg"), 0), true), ((None, 0), false)]);
}

#[test]
fn a_rule_names_only_addresses_its_node_routes_to_the_workload_matched() {
    let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
    let members: BTreeMap<Uuid, Vec<IpNet>> = [
        (a, vec!["10.48.0.0/22".parse().unwrap()]),
        (b, vec!["10.48.4.0/22".parse().unwrap(), "fd00:0:2::/64".parse().unwrap()]),
    ]
    .into();
    let routes = Routes { node: a, members: &members };
    let (web, batch) = (
        BTreeMap::from([("role".to_owned(), "web".to_owned())]),
        BTreeMap::from([("role".to_owned(), "batch".to_owned())]),
    );
    let at = |n: u128, node: Option<Uuid>, addresses: &[&str]| Located {
        id: Uuid::from_u128(n),
        labels: &web,
        node,
        addresses: addresses.iter().map(|x| ip(x)).collect(),
    };
    let workloads = vec![
        at(10, Some(a), &["10.48.0.2", "2001:db8:a::2"]),
        at(11, Some(b), &["10.48.4.2", "fd00:0:2::2"]),
        // On a node outside the mesh, on the old default, which may be one A gives its own.
        at(12, Some(c), &["10.47.0.2", "2001:db8:c::2"]),
        at(13, None, &[]),
        at(14, Some(b), &[]),
        Located { id: Uuid::from_u128(15), labels: &batch, node: Some(a), addresses: vec![] },
    ];
    let resolved = resolve(&body(), &workloads, routes);
    let allow = &resolved[0];
    assert_eq!(allow.place, RulePlace { endpoint: Some("pg".into()), index: 0 });
    let named = ["10.48.0.2", "10.48.4.2", "2001:db8:a::2", "fd00:0:2::2"].map(ip);
    assert_eq!(allow.addresses, named, "its own node's, and the paired member's routed pools'");
    assert_eq!(allow.matched.len(), 5, "batch is not matched");
    let left: Vec<(u128, LeftOut)> =
        allow.left_out.iter().map(|(w, why)| (w.as_u128(), *why)).collect();
    assert_eq!(
        left,
        [(12, LeftOut::Unrouted), (13, LeftOut::NotPlaced), (14, LeftOut::NotObserved)],
        "an allow never names another node's private address it does not route"
    );
    let deny = &resolved[1];
    assert!(deny.addresses.contains(&ip("2001:db8:c::2")), "a deny names a global address too");
    assert!(!deny.addresses.contains(&ip("10.47.0.2")), "and never a private one unrouted");

    let outside = BTreeMap::from([(b, members[&b].clone())]);
    let lone = resolve(&body(), &workloads, Routes { node: a, members: &outside });
    assert_eq!(lone[0].addresses, ["10.48.0.2", "2001:db8:a::2"].map(ip), "A is not a member");
}
