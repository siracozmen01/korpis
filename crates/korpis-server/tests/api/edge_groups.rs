// SPDX-License-Identifier: AGPL-3.0-or-later

//! Edge groups (§3.1 of docs/07-networking.md, trace 78 of docs/23-walkthroughs.md), the control
//! plane's half: a group is its organization's own nodes, an edge pool names a group of its own
//! organization, and a group is deleted only once no pool names it; a workload's `stable`
//! endpoints, where a group of two serves it, are placed on a node reaching every member through
//! the mesh, hold an address and port of the group's pool, a port per transport, and no port of
//! their node; and every member is told the group's table, the workload's node each member's
//! tunnel and what it may reach.

use crate::across::running_at;
use crate::agent::{Stream, TestAgent, next_edges};
use crate::dependencies::{declare, placement};
use crate::domains::child;
use crate::egress::assigned;
use crate::mesh::{opened, side};
use crate::recipes::{body, project, push_paper};
use crate::support::{Harness, mutation, reason};
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;

pub(crate) fn asked(
    key: &str,
    org: &str,
    name: &str,
    members: &[&str],
) -> pb::CreateEdgeGroupRequest {
    let group = pb::EdgeGroup {
        organization: org.into(),
        name: name.into(),
        members: members.iter().map(|m| (*m).to_owned()).collect(),
        ..Default::default()
    };
    pb::CreateEdgeGroupRequest {
        mutation: mutation(key).into(),
        group: group.into(),
        ..Default::default()
    }
}

async fn refusal(h: &Harness, req: pb::CreateEdgeGroupRequest) -> String {
    reason(&h.edges(h.root()).create_edge_group(req).await.unwrap_err())
}

pub(crate) async fn made(h: &Harness, req: pb::CreateEdgeGroupRequest) -> pb::EdgeGroup {
    let out = h.edges(h.root()).create_edge_group(req).await.unwrap().into_owned();
    out.group.into_option().unwrap()
}

/// An edge pool of `cidr` for `group`, on the link its members share, its gateway reserved.
pub(crate) fn edge_pool(
    key: &str,
    org: &str,
    cidr: &str,
    group: &str,
) -> pb::CreateAddressPoolRequest {
    let pool = pb::AddressPool {
        organization: org.into(),
        cidr: cidr.into(),
        scope: pb::AddressPoolScope::ADDRESS_POOL_SCOPE_EDGE.into(),
        group: group.into(),
        interface: "eth0".into(),
        reserved: vec![cidr.replace(".0/29", ".1").replace(".8/29", ".9")],
        ..Default::default()
    };
    pb::CreateAddressPoolRequest {
        mutation: mutation(key).into(),
        pool: pool.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_group_is_its_organization_s_own_nodes_and_goes_once_no_pool_names_it() {
    let h = Harness::new().await;
    let reseller = child(&h, "pinecone").await;
    let reseller_id = Id::parse(IdKind::Organization, &reseller).unwrap().uuid();
    let (e1, e2) = (h.node(None, "edge-1", &[]).await, h.node(None, "edge-2", &[]).await);
    let theirs = h.node(Some(reseller_id), "edge-r", &[]).await;
    let root = h.root_org();

    let foreign = asked("g0", &root, "edge", &[&e1, &theirs]);
    assert_eq!(refusal(&h, foreign).await, "request.invalid", "a reseller's node");
    let g = made(&h, asked("g1", &root, "edge", &[&e1, &e2])).await;
    let settings = (g.first_port, g.last_port, g.probe_port, g.probe_misses);
    assert_eq!(settings, (1024, 65_534, 65_535, 3), "its defaults");
    assert_eq!(refusal(&h, asked("g2", &root, "edge", &[&e2])).await, "edge_group.name_taken");

    let pools = h.pools(h.root());
    let crossing = edge_pool("p0", &reseller, "198.51.100.8/29", &g.id);
    let e = pools.create_address_pool(crossing).await.unwrap_err();
    assert_eq!(reason(&e), "request.invalid", "a group of another organization");
    let pool = pools.create_address_pool(edge_pool("p1", &root, "198.51.100.0/29", &g.id));
    let pool = pool.await.unwrap().into_owned().pool.into_option().unwrap();
    assert_eq!(
        (pool.group.as_str(), pool.routing.as_known()),
        (g.id.as_str(), Some(pb::AddressRouting::ADDRESS_ROUTING_STATIC))
    );

    let edges = h.edges(h.root());
    let delete = |key: &str| pb::DeleteEdgeGroupRequest {
        mutation: mutation(key).into(),
        id: g.id.clone(),
        ..Default::default()
    };
    let named = edges.delete_edge_group(delete("d1")).await.unwrap_err();
    assert_eq!(reason(&named), "edge_group.named");
    let gone = pb::DeleteAddressPoolRequest {
        mutation: mutation("d2").into(),
        id: pool.id.clone(),
        ..Default::default()
    };
    pools.delete_address_pool(gone).await.unwrap();
    edges.delete_edge_group(delete("d3")).await.expect("no pool names it now");
}

/// `workload`'s endpoints as its status shows them, once they are placed: as its assignment is
/// computed, after the placement itself (§3 of docs/07-networking.md), within ten seconds.
pub(crate) async fn endpoints_shown(h: &Harness, workload: &str) -> Vec<pb::Endpoint> {
    for _ in 0..100 {
        let get = pb::GetWorkloadRequest { id: workload.to_owned(), ..Default::default() };
        let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
        let shown = got.status.into_option().unwrap().endpoints;
        if !shown.is_empty() {
            return shown;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("its endpoints were never placed");
}

/// A workload whose game and query endpoints are `stable`, its RCON not reached.
pub(crate) fn arena_body(repo: &str) -> pb::IntentBody {
    let at = |name: &str, exposure: pb::Exposure| pb::EndpointSpec {
        name: name.into(),
        exposure: exposure.into(),
        ..Default::default()
    };
    let endpoints = vec![
        at("game", pb::Exposure::EXPOSURE_STABLE),
        at("query", pb::Exposure::EXPOSURE_STABLE),
        at("rcon", pb::Exposure::EXPOSURE_NONE),
    ];
    pb::IntentBody { endpoints, ..body(repo, &[]) }
}

/// The edges `s` is told once `done` holds of them.
pub(crate) async fn edges_once(
    s: &mut Stream,
    done: impl Fn(&agent::Edges) -> bool,
) -> agent::Edges {
    for _ in 0..30 {
        let told = next_edges(s).await;
        if done(&told) {
            return told;
        }
    }
    panic!("the edges told never came to hold");
}

/// Has `node` advertise `address`: a member's own address, which the control plane probes it
/// at, here on the loopback so no probe leaves this machine.
pub(crate) async fn advertise(h: &Harness, node: &str, address: &str) {
    let id = Id::parse(IdKind::Node, node).unwrap();
    let store = h.db.store().await.unwrap();
    sqlx::query("UPDATE node SET addresses = ARRAY[$2::text] WHERE id = $1")
        .bind(id.uuid())
        .bind(address)
        .execute(store.pool())
        .await
        .unwrap();
}

pub(crate) async fn cordon(h: &Harness, node: &str) {
    let r = pb::CordonNodeRequest {
        mutation: mutation(&format!("cordon-{node}")).into(),
        node: node.to_owned(),
        ..Default::default()
    };
    h.nodes(h.root()).cordon_node(r).await.unwrap();
}

#[tokio::test]
async fn a_stable_endpoint_holds_its_group_s_address_and_every_member_forwards_it() {
    let h = Harness::new().await;
    let (e1, e2, host) = (
        TestAgent::enroll(&h, "edge-1", 81).await,
        TestAgent::enroll(&h, "edge-2", 82).await,
        TestAgent::enroll(&h, "host-a", 83).await,
    );
    // Each reports a fresh handshake with the other two.
    let mut s1 = opened(&h, &e1, side(1, "10.48.0.0/22", &[2, 3])).await;
    let _s2 = opened(&h, &e2, side(2, "10.48.4.0/22", &[1, 3])).await;
    let mut sa = opened(&h, &host, side(3, "10.48.8.0/22", &[1, 2])).await;
    cordon(&h, &e1.node).await;
    cordon(&h, &e2.node).await;
    advertise(&h, &e1.node, "127.0.0.81").await;
    advertise(&h, &e2.node, "127.0.0.82").await;
    let root = h.root_org();
    let group = made(&h, asked("g1", &root, "edge", &[&e1.node, &e2.node])).await;
    let pool = edge_pool("p1", &root, "198.51.100.0/29", &group.id);
    h.pools(h.root()).create_address_pool(pool).await.unwrap();

    let repo = push_paper().await;
    let games = project(&h, &root, "games").await;
    let arena = declare(&h, &games, "arena", arena_body(&repo)).await;
    assert_eq!(placement(&h, &arena.id).await.node, host.node, "the node reaching both members");
    let shown = endpoints_shown(&h, &arena.id).await;
    let at = |e: &pb::Endpoint| e.address.as_option().map(|a| (a.ips.clone(), a.port));
    let reached: Vec<_> = shown.iter().map(|e| (e.name.as_str(), at(e))).collect();
    let stable = Some((vec!["198.51.100.2".to_owned()], 25_565));
    // The game's TCP port and the query's UDP port share a number, a port per transport.
    assert_eq!(reached[..2], [("game", stable.clone()), ("query", stable)], "{reached:?}");
    let game = shown[0].address.as_option().unwrap();
    assert_eq!(game.edge_group, group.id, "its group named");
    assert!(game.unreached.contains("holds its address"), "nobody holds it: {}", game.unreached);
    let told = assigned(&h, &arena.id).await;
    let game = told.endpoints.iter().find(|e| e.name == "game").unwrap();
    assert_eq!(game.exposure.as_known(), Some(pb::Exposure::EXPOSURE_STABLE));
    assert!(game.address.as_option().is_none(), "no port of its node");

    running_at(&mut sa, &arena.id, "10.48.8.2").await;
    let forwarded = edges_once(&mut s1, |e| {
        e.groups.first().is_some_and(|g| g.forwards.iter().any(|f| !f.node.is_empty()))
    })
    .await;
    assert_eq!(forwarded.mesh_address, "10.48.3.255", "the last of its pool");
    let table = &forwarded.groups[0];
    assert_eq!((table.probe_port, table.probe_node.as_str()), (65_535, host.node.as_str()));
    let f = table
        .forwards
        .iter()
        .find(|f| f.protocol.as_known() == Some(pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP))
        .unwrap();
    let to = (f.address.as_str(), f.port, f.mesh_address.as_str(), f.workload_address.as_str());
    assert_eq!(to, ("198.51.100.2", 25_565, "10.48.11.255", "10.48.8.2"));

    let behind = edges_once(&mut sa, |e| e.sources.len() == 2).await;
    let tunnels: Vec<&str> = behind.sources.iter().map(|s| s.mesh_address.as_str()).collect();
    assert!(tunnels.contains(&"10.48.3.255") && tunnels.contains(&"10.48.7.255"), "{tunnels:?}");
    let admits: Vec<(&str, u32)> = behind.sources[0]
        .admits
        .iter()
        .map(|a| (a.workload_address.as_str(), a.container_port))
        .collect();
    assert!(admits.contains(&("10.48.8.2", 25_565)) && admits.contains(&("10.48.11.255", 65_535)));
}
