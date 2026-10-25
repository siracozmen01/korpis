// SPDX-License-Identifier: AGPL-3.0-or-later

//! The mesh between nodes as the control plane keeps it (§9.1 of docs/07-networking.md): each
//! node that reports its side is told every other member, with its key, endpoint, and pools; a
//! node reporting a pool it was not given is told it is outside, and why, since every other member
//! would route that pool to it; and
//! `Node.overlay` is available only once both sides of every pair with the node report a fresh
//! handshake, the node showing its key, port, pools, and each pair with it.

use crate::agent::{Stream, TestAgent, next_peers};
use crate::support::Harness;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A node's side of the mesh: its key `seed` repeated, its `pool`, and a handshake now with each
/// of the keys `reached` names.
pub(crate) fn side(seed: u8, pool: &str, reached: &[u8]) -> agent::OverlayFacts {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let handshakes = reached.iter().map(|k| agent::OverlayHandshake {
        public_key: vec![*k; 32],
        latest_unix_seconds: i64::try_from(now).unwrap(),
        ..Default::default()
    });
    agent::OverlayFacts {
        public_key: vec![seed; 32],
        port: 51820,
        pools: vec![pool.to_owned()],
        handshakes: handshakes.collect(),
        ..Default::default()
    }
}

/// `node`'s session, opened with a Hello reporting `overlay`.
pub(crate) async fn opened(h: &Harness, node: &TestAgent, overlay: agent::OverlayFacts) -> Stream {
    let mut hello = node.hello_at(SystemTime::now(), node.node.as_bytes());
    if let Some(Up::Hello(hello)) = hello.message.as_mut() {
        hello.overlay = overlay.into();
    }
    let (stream, first) = TestAgent::open(h, hello).await;
    first.unwrap();
    stream
}

/// A heartbeat reporting `overlay`.
fn beat(overlay: agent::OverlayFacts) -> agent::AgentMessage {
    let mut beat = TestAgent::heartbeat();
    if let Some(Up::NodeStatus(status)) = beat.message.as_mut() {
        status.overlay = overlay.into();
    }
    beat
}

/// Whether `node` is in the mesh and reaches every other member, as the API says.
async fn available(h: &Harness, node: &str) -> bool {
    got(h, node).await.overlay == pb::Overlay::OVERLAY_AVAILABLE
}

/// `node` as the API shows it.
async fn got(h: &Harness, node: &str) -> pb::Node {
    let nodes = h.nodes(h.root());
    let request = pb::GetNodeRequest { id: node.to_owned(), ..Default::default() };
    let got = nodes.get_node(request).await.unwrap().into_owned();
    got.node.into_option().unwrap()
}

#[tokio::test]
async fn each_member_is_told_the_others_a_node_claiming_a_pool_why_and_a_pair_judged_by_both() {
    let h = Harness::new().await;
    let (a, b) =
        (TestAgent::enroll(&h, "mesh-a", 41).await, TestAgent::enroll(&h, "mesh-b", 42).await);
    let mut sa = opened(&h, &a, side(1, "10.48.0.0/22", &[])).await;
    let mut sb = opened(&h, &b, side(2, "10.48.4.0/22", &[])).await;
    // A session may be told the mesh before the other node reported; it is told again.
    let mut told = next_peers(&mut sa).await;
    while told.peers.is_empty() {
        told = next_peers(&mut sa).await;
    }
    assert_eq!(told.peers.len(), 1);
    let peer = &told.peers[0];
    assert_eq!(
        (peer.node.as_str(), peer.public_key.as_slice(), peer.endpoint.as_str()),
        (b.node.as_str(), [2; 32].as_slice(), "203.0.113.10:51820"),
        "B's key, and its first advertised address on its port",
    );
    assert_eq!(peer.allowed_ips, ["10.48.4.0/22"]);
    assert!(!available(&h, &a.node).await, "no handshake yet");

    let c = TestAgent::enroll(&h, "mesh-c", 43).await;
    let mut sc = opened(&h, &c, side(3, "10.48.0.0/24", &[])).await;
    let outside = next_peers(&mut sc).await;
    assert!(outside.peers.is_empty());
    assert!(
        outside.outside.contains("10.48.0.0/24, which it was not given"),
        "{}",
        outside.outside
    );

    sa.send(beat(side(1, "10.48.0.0/22", &[2]))).await.unwrap();
    sb.send(beat(side(2, "10.48.4.0/22", &[1]))).await.unwrap();
    for _ in 0..50 {
        if available(&h, &a.node).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(available(&h, &a.node).await, "both sides report a fresh handshake");
    let shown = got(&h, &a.node).await;
    assert_eq!((shown.overlay_key, shown.overlay_port), (vec![1; 32], 51820));
    assert_eq!(shown.pools, ["10.48.0.0/22"]);
    let peers: Vec<(&str, bool)> =
        shown.peers.iter().map(|p| (p.node.as_str(), p.reachable)).collect();
    assert_eq!(peers, [(b.node.as_str(), true)], "C is outside, and no peer of A's");
    assert!(!available(&h, &c.node).await, "outside the mesh");
}
