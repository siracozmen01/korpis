// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's probe of an edge group's members, and failover by it (§3.1 of
//! docs/07-networking.md, steps 6 and 7 of trace 78 in docs/23-walkthroughs.md). Each member's
//! own address is on the loopback here, and the test answers a probe there as the node behind
//! the group would once the member's forwarding carried it: signed with that node's key. A member
//! answered is ready and named to hold the address; one whose probes go unanswered is failing,
//! and shown with why. When the holder stops answering and the other member answers, the other
//! is named instead, and is granted the address once the holder has released it.

use crate::across::running_at;
use crate::agent::{Stream, TestAgent, any};
use crate::dependencies::declare;
use crate::edge_groups::{advertise, arena_body, asked, cordon, edge_pool, edges_once, made};
use crate::issuance::send;
use crate::mesh::{opened, side};
use crate::recipes::{project, push_paper};
use crate::support::Harness;
use ed25519_dalek::SigningKey;
use korpis_core::edge::probe::{answer_message, read_request};
use korpis_core::node::identity::sign;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

const ADDRESS: &str = "198.51.100.2";

/// Answers every probe at `at` as `node` with `key`, as the node behind the group would.
pub(crate) async fn answering(
    at: (&'static str, u16),
    (node, key): (String, SigningKey),
) -> JoinHandle<()> {
    let socket = UdpSocket::bind(at).await.unwrap();
    tokio::spawn(async move {
        let mut buffer = [0; 65];
        while let Ok((n, from)) = socket.recv_from(&mut buffer).await {
            let Some((nonce, group, address)) = buffer.get(..n).and_then(read_request) else {
                continue;
            };
            let signed = sign(&key, &answer_message(group, address, &node, &nonce));
            let _ = socket.send_to(&signed, from).await;
        }
    })
}

/// The group as read once `done` holds of it, within ten seconds.
pub(crate) async fn group_once(
    h: &Harness,
    id: &str,
    done: impl Fn(&pb::EdgeGroup) -> bool,
) -> pb::EdgeGroup {
    let client = pb::EdgeGroupServiceClient::new(h.http.clone(), h.root());
    for _ in 0..100 {
        let r = pb::GetEdgeGroupRequest { id: id.into(), ..Default::default() };
        let g = client.get_edge_group(r).await.unwrap().into_owned().group.into_option().unwrap();
        if done(&g) {
            return g;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the group's status never came to hold");
}

fn ready(g: &pb::EdgeGroup, node: &str) -> Option<bool> {
    g.member_status.iter().find(|m| m.node == node).map(|m| m.ready)
}

fn ask(nonce: u8) -> Up {
    Up::EdgeLeaseRequest(Box::new(agent::EdgeLeaseRequest {
        address: ADDRESS.into(),
        nonce: vec![nonce],
        ..Default::default()
    }))
}

/// The next answer to an edge lease request on `s`: the epoch granted, or none.
async fn granted(s: &mut Stream) -> Option<u64> {
    loop {
        match any(s).await.message {
            Some(Down::EdgeLeaseGrant(g)) => return Some(g.epoch),
            Some(Down::EdgeLeaseRefusal(_)) => return None,
            _ => {}
        }
    }
}

fn holds(told: &agent::Edges) -> bool {
    told.groups.iter().flat_map(|g| &g.hold).any(|h| h.address == ADDRESS)
}

#[tokio::test]
async fn a_member_answering_its_probe_holds_the_address_and_one_that_stops_gives_it_up() {
    let h = Harness::new().await;
    let (e1, e2, host) = (
        TestAgent::enroll(&h, "edge-1", 94).await,
        TestAgent::enroll(&h, "edge-2", 95).await,
        TestAgent::enroll(&h, "host-a", 96).await,
    );
    let mut s1 = opened(&h, &e1, side(1, "10.48.0.0/22", &[2, 3])).await;
    let mut s2 = opened(&h, &e2, side(2, "10.48.4.0/22", &[1, 3])).await;
    let mut sa = opened(&h, &host, side(3, "10.48.8.0/22", &[1, 2])).await;
    for (member, at) in [(&e1, "127.0.0.94"), (&e2, "127.0.0.95")] {
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
    h.pools(h.root())
        .create_address_pool(edge_pool("p1", &root, "198.51.100.0/29", &group.id))
        .await
        .unwrap();
    let games = project(&h, &root, "games").await;
    let arena = declare(&h, &games, "arena", arena_body(&push_paper().await)).await;
    running_at(&mut sa, &arena.id, "10.48.8.2").await;

    let behind = (host.node.clone(), host.key.clone());
    let first = answering(("127.0.0.94", port), behind.clone()).await;
    let shown = group_once(&h, &group.id, |g| {
        ready(g, &e1.node) == Some(true) && ready(g, &e2.node) == Some(false)
    })
    .await;
    let failing = shown.member_status.iter().find(|m| m.node == e2.node).unwrap();
    assert!(failing.reason.contains("missed 3 probes"), "{}", failing.reason);
    assert!(holds(&edges_once(&mut s1, holds).await), "the ready member is named");
    send(&mut s1, ask(1)).await;
    let epoch = granted(&mut s1).await.expect("granted to the ready member");

    first.abort();
    let _second = answering(("127.0.0.95", port), behind).await;
    group_once(&h, &group.id, |g| {
        ready(g, &e1.node) == Some(false) && ready(g, &e2.node) == Some(true)
    })
    .await;
    assert!(holds(&edges_once(&mut s2, holds).await), "named once the holder stops answering");
    send(&mut s2, ask(2)).await;
    assert_eq!(granted(&mut s2).await, None, "not before the holder lets go");
    let released =
        agent::EdgeLeaseReleased { address: ADDRESS.into(), epoch, ..Default::default() };
    send(&mut s1, Up::EdgeLeaseReleased(Box::new(released))).await;
    // The release and the next request travel on two sessions, which nothing orders.
    group_once(&h, &group.id, |g| g.holdings.iter().all(|a| a.node.is_empty())).await;
    send(&mut s2, ask(3)).await;
    assert!(granted(&mut s2).await.is_some_and(|e| e > epoch), "the next epoch, to the other");
    let shown = group_once(&h, &group.id, |g| g.holdings.iter().any(|a| a.node == e2.node)).await;
    assert_eq!(shown.holdings[0].address, ADDRESS);
}
