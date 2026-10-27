// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;
use crate::edge::{
    DEFAULT_PORTS, DEFAULT_PROBE_INTERVAL, DEFAULT_PROBE_MISSES, DEFAULT_PROBE_PORT, DEFAULT_TERM,
};
use std::time::UNIX_EPOCH;

const GROUP: Uuid = Uuid::from_u128(10);
const E1: Uuid = Uuid::from_u128(1);
const E2: Uuid = Uuid::from_u128(2);
const A: Uuid = Uuid::from_u128(3);
const B: Uuid = Uuid::from_u128(4);

fn group(members: &[Uuid]) -> EdgeGroup {
    EdgeGroup {
        id: GROUP,
        organization: Uuid::from_u128(9),
        name: "edge".into(),
        members: members.to_vec(),
        ports: DEFAULT_PORTS,
        probe_port: DEFAULT_PROBE_PORT,
        probe_interval: DEFAULT_PROBE_INTERVAL,
        probe_misses: DEFAULT_PROBE_MISSES,
        term: DEFAULT_TERM,
        created_at: UNIX_EPOCH,
    }
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

fn v4(s: &str) -> Ipv4Addr {
    s.parse().unwrap()
}

fn entry(port: u16, on: Option<Uuid>) -> Entry {
    Entry {
        group: GROUP,
        address: ip("198.51.100.2"),
        port,
        protocol: Protocol::Udp,
        target: on.map(|node| Target {
            node,
            workload_address: ip("10.48.4.2"),
            container_port: 25_565,
        }),
    }
}

/// Each of `nodes` in the mesh, the last but one with no unique local prefix.
fn mesh(nodes: &[Uuid]) -> BTreeMap<Uuid, Mesh> {
    let at = |n: &Uuid| Mesh {
        v4: v4(&format!("10.48.{}.255", n.as_u128())),
        v6: (*n != B)
            .then(|| format!("fd00:0:0:{}:ffff:ffff:ffff:ffff", n.as_u128()).parse().unwrap()),
    };
    nodes.iter().map(|n| (*n, at(n))).collect()
}

fn at(nodes: &BTreeMap<Uuid, Mesh>, node: Uuid) -> Mesh {
    nodes[&node]
}

static APART: BTreeSet<Uuid> = BTreeSet::new();

/// `addresses` as the tables see the mesh, no control plane sharing a member's network.
fn view(addresses: &BTreeMap<Uuid, Mesh>) -> MeshView<'_> {
    MeshView { addresses, colocated: &APART }
}

#[test]
fn every_member_forwards_the_whole_group_and_the_node_behind_admits_each_member() {
    let all = mesh(&[E1, E2, A]);
    let holding = Holding::from([(ip("198.51.100.2"), (E2, "eth1".to_owned()))]);
    let entries = [entry(25_565, Some(A)), entry(25_575, None)];
    let t = tables(&[group(&[E1, E2])], Served::stable(&entries), view(&all), &holding);
    assert!(t[&E1].groups[0].hold.is_empty());
    assert_eq!(t[&E2].groups[0].hold, [(ip("198.51.100.2"), "eth1".to_owned())], "one holds it");
    for edge in [E1, E2] {
        let f = &t[&edge].groups[0];
        assert_eq!(f.forwards.len(), 2, "the whole table, whichever holds the address");
        let (target, mesh_address) = f.forwards[0].target.clone().unwrap();
        assert_eq!((target.node, mesh_address), (A, Some(v4("10.48.3.255"))));
        assert!(f.forwards[1].target.is_none(), "not running: refused");
        assert_eq!(f.probe, Some((A, at(&all, A))), "the probe crosses the same link");
    }
    let behind = &t[&A];
    assert_eq!(behind.mesh, Some(at(&all, A)));
    let from: Vec<Uuid> = behind.sources.iter().map(|s| s.node).collect();
    assert_eq!(from, [E1, E2]);
    let admits = &behind.sources[0].admits;
    let game = Admit { address: ip("10.48.4.2"), port: 25_565, protocol: Protocol::Udp };
    let probe = |a: &str| Admit { address: ip(a), port: 65_535, protocol: Protocol::Udp };
    let probe6 = probe("fd00:0:0:3:ffff:ffff:ffff:ffff");
    assert_eq!(admits, &[probe("10.48.3.255"), game, probe6], "sorted, IPv6 last");
    assert!(!t.contains_key(&B), "a node nothing forwards to is told nothing");
}

#[test]
fn a_workload_on_a_member_needs_no_tunnel_and_a_lone_member_or_one_outside_forwards_nothing() {
    let none = Holding::new();
    let pair = mesh(&[E1, E2]);
    let t =
        tables(&[group(&[E1, E2])], Served::stable(&[entry(25_565, Some(E1))]), view(&pair), &none);
    let own = &t[&E1].groups[0];
    assert_eq!(own.forwards[0].target.as_ref().unwrap().1, None, "on the member itself");
    assert_eq!(own.probe, Some((E2, at(&pair, E2))), "probed through the other member");
    let from_e2: Vec<Uuid> = t[&E1].sources.iter().map(|s| s.node).collect();
    assert_eq!(from_e2, [E2], "the other member tunnels to it");

    let lone_mesh = mesh(&[E1, A]);
    let lone =
        tables(&[group(&[E1])], Served::stable(&[entry(25_565, Some(A))]), view(&lone_mesh), &none);
    assert!(lone.is_empty(), "one machine serves nothing");
    let outside =
        tables(&[group(&[E1, E2])], Served::stable(&[entry(25_565, Some(B))]), view(&pair), &none);
    assert!(outside[&E1].groups[0].forwards[0].target.is_none(), "its node is not in the mesh");
}

#[test]
fn the_probe_goes_through_a_node_no_control_plane_shares_a_network_with_where_there_is_one() {
    let all = mesh(&[E1, E2, A, B]);
    let none = Holding::new();
    let entries = [entry(25_565, Some(A)), entry(25_575, Some(B))];
    let near = BTreeSet::from([A]);
    let seen = MeshView { addresses: &all, colocated: &near };
    let t = tables(&[group(&[E1, E2])], Served::stable(&entries), seen, &none);
    assert_eq!(t[&E1].groups[0].probe.map(|(n, _)| n), Some(B), "A's answer would stay local");
    let both = BTreeSet::from([A, B, E2]);
    let crowded = MeshView { addresses: &all, colocated: &both };
    let t = tables(&[group(&[E1, E2])], Served::stable(&entries), crowded, &none);
    assert_eq!(t[&E1].groups[0].probe.map(|(n, _)| n), Some(A), "none apart: the first still");
}

/// An `ingress` endpoint of the group, its workload on `on` at 10.48.4.3.
fn proxied(on: Option<Uuid>) -> Proxied {
    let endpoint = Endpoint {
        id: Uuid::from_u128(20),
        workload: Uuid::from_u128(21),
        name: "web".into(),
        protocol: Protocol::Http,
        container_port: 8080,
        exposure: crate::workload::spec::endpoints::Exposure::Ingress,
        node: on.unwrap_or(A),
        port: None,
        domains: vec!["shop.example.com".into()],
        tls: None,
        certificate: None,
    };
    let target =
        on.map(|node| Target { node, workload_address: ip("10.48.4.3"), container_port: 8080 });
    Proxied { group: GROUP, endpoint, target }
}

#[test]
fn every_member_routes_the_group_s_names_at_its_ingress_address_and_the_node_admits_its_proxy() {
    let all = mesh(&[E1, E2, A]);
    let ingress = [IngressAddress { address: ip("198.51.100.1"), group: GROUP, pool: GROUP }];
    let holding = Holding::from([(ip("198.51.100.1"), (E1, "eth1".to_owned()))]);
    let routes = [proxied(Some(A)), proxied(Some(B)), proxied(None)];
    let served = Served { entries: &[], ingress: &ingress, proxied: &routes };
    let t = tables(&[group(&[E1, E2])], served, view(&all), &holding);
    for edge in [E1, E2] {
        let f = &t[&edge].groups[0];
        assert_eq!(f.ingress, [ip("198.51.100.1")]);
        let to: Vec<Option<Uuid>> =
            f.proxied.iter().map(|p| p.target.as_ref().map(|t| t.node)).collect();
        assert_eq!(to, [Some(A), None, None], "a node outside the mesh, or none: no route");
    }
    assert_eq!(t[&E1].groups[0].hold, [(ip("198.51.100.1"), "eth1".to_owned())], "held as any");
    let behind = &t[&A];
    let web = Admit { address: ip("10.48.4.3"), port: 8080, protocol: Protocol::Tcp };
    for s in &behind.sources {
        assert_eq!(s.proxied, std::slice::from_ref(&web), "each member's proxy, through the mesh");
        assert!(s.admits.is_empty(), "nothing through a GRE link");
    }
    let own = tables(
        &[group(&[E1, E2])],
        served_on(&ingress, &[proxied(Some(E1))]),
        view(&all),
        &holding,
    );
    let from: Vec<Uuid> = own[&E1].sources.iter().map(|s| s.node).collect();
    assert_eq!(from, [E2], "the other member's proxy; its own reaches it on its link");
    assert_eq!(own[&E2].groups[0].proxied[0].target.as_ref().map(|t| t.node), Some(E1));
}

fn served_on<'a>(ingress: &'a [IngressAddress], proxied: &'a [Proxied]) -> Served<'a> {
    Served { entries: &[], ingress, proxied }
}
