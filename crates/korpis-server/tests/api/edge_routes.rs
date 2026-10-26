// SPDX-License-Identifier: AGPL-3.0-or-later

//! A routed edge pool (§3.1 of docs/07-networking.md, step 7 of trace 78 in
//! docs/23-walkthroughs.md): its address is routed by the pool's router, written with the SDK, to
//! the member its lease was issued to, at that member's advertised address, once the lease is
//! granted; when the lease moves to another member the router is asked for that member; a router
//! failing is shown on the group with its reason and on the workload's endpoint as why it is not
//! reached, until the router answers when it is asked again.

use crate::across::running_at;
use crate::address_routes::{Fabric, serving};
use crate::agent::{Stream, TestAgent, any};
use crate::dependencies::declare;
use crate::edge_groups::{advertise, arena_body, asked, cordon, edge_pool, made};
use crate::edge_probes::{answering, group_once};
use crate::issuance::send;
use crate::mesh::{opened, side};
use crate::recipes::{project, push_paper};
use crate::support::Harness;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::sync::atomic::Ordering;
use std::time::Duration;

const ADDRESS: &str = "198.51.100.2";

/// The lease on `ADDRESS` asked for on `s` with `nonce`, granted within the group's probe of the
/// member: its epoch.
async fn lease(s: &mut Stream, nonce: u8) -> u64 {
    for _ in 0..50 {
        let asked = agent::EdgeLeaseRequest {
            address: ADDRESS.into(),
            nonce: vec![nonce],
            ..Default::default()
        };
        send(s, Up::EdgeLeaseRequest(Box::new(asked))).await;
        loop {
            match any(s).await.message {
                Some(Down::EdgeLeaseGrant(g)) => return g.epoch,
                Some(Down::EdgeLeaseRefusal(_)) => break,
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("the lease was never granted");
}

/// That `fabric`'s router was last asked to route `ADDRESS` to `node`, within forty seconds.
async fn routed_to(fabric: &Fabric, node: &str) {
    let wanted = (ADDRESS.to_owned(), node.to_owned());
    for _ in 0..200 {
        if fabric.routes().last() == Some(&wanted) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("never routed to {node}: {:?}", fabric.routes());
}

/// Why the workload's first endpoint is not reached, as its status says.
async fn unreached(h: &Harness, workload: &str) -> String {
    let get = pb::GetWorkloadRequest { id: workload.to_owned(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    let shown = got.status.into_option().unwrap().endpoints;
    shown[0].address.as_option().map(|a| a.unreached.clone()).unwrap_or_default()
}

#[tokio::test]
async fn a_routed_edge_pool_s_address_is_routed_to_the_member_holding_its_lease() {
    let h = Harness::new().await;
    // The router's node is enrolled first, and so given the mesh's first pool.
    let fabric = Fabric::default();
    let _router = serving(&h, fabric.clone()).await;
    let (e1, e2, host) = (
        TestAgent::enroll(&h, "edge-1", 97).await,
        TestAgent::enroll(&h, "edge-2", 98).await,
        TestAgent::enroll(&h, "host-a", 99).await,
    );
    let mut s1 = opened(&h, &e1, side(1, "10.48.4.0/22", &[2, 3])).await;
    let mut s2 = opened(&h, &e2, side(2, "10.48.8.0/22", &[1, 3])).await;
    let mut sa = opened(&h, &host, side(3, "10.48.12.0/22", &[1, 2])).await;
    for (member, at) in [(&e1, "127.0.0.97"), (&e2, "127.0.0.98")] {
        cordon(&h, &member.node).await;
        advertise(&h, &member.node, at).await;
    }
    let port = std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let root = h.root_org();
    let mut fast = asked("g1", &root, "edge", &[&e1.node, &e2.node]);
    let mut g = fast.group.as_option().cloned().unwrap();
    (g.first_port, g.last_port, g.probe_port) = (1024, 1999, u32::from(port));
    g.probe_interval = korpis_wire::duration(Duration::from_millis(200)).into();
    fast.group = g.into();
    let group = made(&h, fast).await;
    let mut routed = edge_pool("p1", &root, "198.51.100.0/29", &group.id);
    let mut pool = routed.pool.as_option().cloned().unwrap();
    (pool.router, pool.interface) = ("bgp".into(), String::new());
    pool.routing = pb::AddressRouting::ADDRESS_ROUTING_BGP.into();
    routed.pool = pool.into();
    h.pools(h.root()).create_address_pool(routed).await.unwrap();
    let games = project(&h, &root, "games").await;
    let arena = declare(&h, &games, "arena", arena_body(&push_paper().await)).await;
    running_at(&mut sa, &arena.id, "10.48.12.2").await;

    let behind = (host.node.clone(), host.key.clone());
    let first = answering(("127.0.0.97", port), behind.clone()).await;
    let epoch = lease(&mut s1, 1).await;
    routed_to(&fabric, &e1.node).await;
    let shown = group_once(&h, &group.id, |g| g.holdings.iter().any(|a| a.routed_to == e1.node));
    assert_eq!(shown.await.holdings[0].router, "bgp");

    fabric.1.store(true, Ordering::SeqCst);
    first.abort();
    let _second = answering(("127.0.0.98", port), behind).await;
    let released =
        agent::EdgeLeaseReleased { address: ADDRESS.into(), epoch, ..Default::default() };
    send(&mut s1, Up::EdgeLeaseReleased(Box::new(released))).await;
    assert!(lease(&mut s2, 2).await > epoch, "the next epoch, to the other member");
    let failing = group_once(&h, &group.id, |g| {
        g.holdings.iter().any(|a| a.node == e2.node && !a.route_failure.is_empty())
    })
    .await;
    let holding = &failing.holdings[0];
    assert!(holding.route_failure.contains("bgp.session_down"), "{holding:?}");
    assert_eq!(holding.routed_to, e1.node, "still where the router last routed it");
    let why = unreached(&h, &arena.id).await;
    assert!(why.contains("router"), "not reached through the member holding it: {why}");

    fabric.1.store(false, Ordering::SeqCst);
    routed_to(&fabric, &e2.node).await;
    let back = group_once(&h, &group.id, |g| g.holdings.iter().any(|a| a.routed_to == e2.node));
    assert_eq!(back.await.holdings[0].route_failure, "");
}
