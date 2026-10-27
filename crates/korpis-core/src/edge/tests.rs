// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;
use crate::address::{DEFAULT_QUARANTINE, Routing, Scope};
use crate::workload::endpoint::Transport;
use crate::workload::endpoint::Transport::{Tcp, Udp};
use std::time::UNIX_EPOCH;

const ORG: Uuid = Uuid::from_u128(1);
const GROUP: Uuid = Uuid::from_u128(2);

fn group() -> EdgeGroup {
    EdgeGroup {
        id: GROUP,
        organization: ORG,
        name: "edge".into(),
        members: vec![Uuid::from_u128(3), Uuid::from_u128(4)],
        ports: DEFAULT_PORTS,
        probe_port: DEFAULT_PROBE_PORT,
        probe_interval: DEFAULT_PROBE_INTERVAL,
        probe_misses: DEFAULT_PROBE_MISSES,
        term: DEFAULT_TERM,
        created_at: UNIX_EPOCH,
    }
}

fn pool(cidr: &str) -> AddressPool {
    AddressPool {
        id: Uuid::from_u128(9),
        organization: ORG,
        cidr: cidr.parse().unwrap(),
        scope: Scope::Edge { group: GROUP, router: None },
        routing: Routing::Static,
        interface: "eth0".into(),
        quarantine: DEFAULT_QUARANTINE,
        reserved: vec!["198.51.100.1".parse().unwrap()],
        created_at: UNIX_EPOCH,
    }
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[test]
fn a_group_says_what_can_hold_and_serves_only_with_two_members() {
    let g = group();
    assert_eq!(g.validate(), Ok(()));
    assert!(g.serves());
    assert_eq!(g.detection(), Duration::from_secs(3), "three missed probes a second apart");
    let alone = EdgeGroup { members: vec![Uuid::from_u128(3)], ..group() };
    assert!(alone.validate().is_ok() && !alone.serves(), "one machine is node_bound");
    let twice = EdgeGroup { members: vec![Uuid::from_u128(3); 2], ..group() };
    assert!(matches!(twice.validate(), Err(GroupError::Member(_))));
    let inside = EdgeGroup { probe_port: 25_565, ..group() };
    assert_eq!(inside.validate(), Err(GroupError::ProbePort(25_565)), "a port it gives");
    let short = EdgeGroup { term: Duration::from_secs(2), ..group() };
    assert_eq!(short.validate(), Err(GroupError::Term));
    let named = EdgeGroup { name: "Edge One".into(), ..group() };
    assert!(matches!(named.validate(), Err(GroupError::Name(_))));
    assert_eq!(EdgeGroup { ports: (0, 10), ..group() }.validate(), Err(GroupError::Ports(0, 10)));
}

#[test]
fn an_edge_pool_is_static_exactly_where_it_names_no_router() {
    assert_eq!(pool("198.51.100.0/28").validate(), Ok(()));
    let routed = AddressPool {
        scope: Scope::Edge { group: GROUP, router: Some("bgp".into()) },
        routing: Routing::Bgp,
        ..pool("198.51.100.0/28")
    };
    assert_eq!(routed.validate(), Ok(()));
    assert_eq!((routed.group(), routed.router()), (Some(GROUP), Some("bgp")));
    let unrouted = AddressPool { routing: Routing::Bgp, ..pool("198.51.100.0/28") };
    assert_eq!(unrouted.validate(), Err(crate::address::PoolError::Routing));
    assert!(!pool("198.51.100.0/28").reaches(Uuid::from_u128(3), ORG), "not a dedicated pool");
}

#[test]
fn a_workload_s_stable_endpoints_share_an_address_on_their_declared_ports_where_one_has_room() {
    let p = pool("198.51.100.0/29");
    let none = TakenPorts::new();
    let ask = |taken: &TakenPorts, wanted: &[(u16, Transport)], prefer: Option<IpAddr>| {
        allocate((&p, taken), wanted, (DEFAULT_PORTS, prefer), at(0))
    };
    let first = ask(&none, &[(25_565, Tcp), (25_565, Udp), (25_575, Tcp)], None).unwrap();
    let same = vec![25_565, 25_565, 25_575];
    assert_eq!((first.address, first.ports), (ip("198.51.100.2"), same), "a port per transport");

    let mut taken = TakenPorts::from([((ip("198.51.100.2"), 25_565, Tcp), Taken::Held(ORG))]);
    let second = ask(&taken, &[(25_565, Tcp)], None).unwrap();
    assert_eq!((second.address, second.ports), (ip("198.51.100.3"), vec![25_565]), "next one");
    let kept = ask(&taken, &[(25_565, Tcp)], Some(ip("198.51.100.2"))).unwrap();
    assert_eq!(kept.address, ip("198.51.100.2"), "its own address, another port");
    assert_eq!(kept.ports, vec![1024]);

    // Its declared port below the range is given one of the range.
    assert_eq!(ask(&none, &[(80, Tcp)], None).unwrap().ports, vec![1024]);
    // Every address holding 25565, the lowest takes the lowest free port of the range.
    for a in p.addresses() {
        taken.insert((a, 25_565, Tcp), Taken::Held(ORG));
    }
    let crowded = ask(&taken, &[(25_565, Tcp)], None).unwrap();
    assert_eq!((crowded.address, crowded.ports), (ip("198.51.100.2"), vec![1024]));
    // A port resting after its holder left is not given until its quarantine has passed.
    let resting = TakenPorts::from([((ip("198.51.100.2"), 25_565, Tcp), Taken::Released(at(0)))]);
    assert_eq!(ask(&resting, &[(25_565, Tcp)], None).unwrap().address, ip("198.51.100.3"));
}

#[test]
fn an_ipv6_address_is_found_beside_on_the_same_ports_and_a_node_s_mesh_address_is_its_last() {
    let mut p = pool("2001:db8::/126");
    p.reserved = Vec::new();
    let taken = TakenPorts::from([((ip("2001:db8::1"), 25_565, Udp), Taken::Held(ORG))]);
    assert_eq!(alongside((&p, &taken), &[(25_565, Udp)], None, at(0)), Some(ip("2001:db8::2")));
    let pools = ["fd00::/64".parse().unwrap(), "10.48.4.0/22".parse().unwrap()];
    assert_eq!(mesh_address(&pools), "10.48.7.255".parse().ok());
    assert_eq!(mesh_address_v6(&pools), "fd00::ffff:ffff:ffff:ffff".parse().ok());
    let routed = ["2001:db8:1::/64".parse().unwrap()];
    assert_eq!(mesh_address_v6(&routed), None, "an operator's routed prefix is not the mesh's");
}

#[test]
fn a_group_s_ingress_address_is_the_lowest_whose_80_and_443_are_free_over_both_transports() {
    let p = pool("198.51.100.0/29");
    let none = TakenPorts::new();
    let first = ingress_room((&p, &none), at(0)).unwrap();
    // A `stable` endpoint's port on it beside its ingress ports leaves it the ingress address.
    let beside = TakenPorts::from([((first, 25_565, Tcp), Taken::Held(ORG))]);
    assert_eq!(ingress_room((&p, &beside), at(0)), Some(first));
    // One holding its 443 over UDP alone moves it to the next.
    let held = TakenPorts::from([((first, 443, Udp), Taken::Held(ORG))]);
    let next = ingress_room((&p, &held), at(0)).unwrap();
    assert_ne!(next, first);
    assert_eq!(ingress_ports().len(), 4, "80 and 443, each over TCP and UDP");
}
