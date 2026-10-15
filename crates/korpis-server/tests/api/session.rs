// SPDX-License-Identifier: AGPL-3.0-or-later

//! Agent sessions (§4.2 and §7 of docs/02-architecture.md): a signed Hello opens one, a
//! replayed, forged, or stale one does not, and a node that stops sending heartbeats is
//! unreachable after three.

use crate::agent::{TestAgent, welcome};
use crate::support::{Harness, detail, reason};
use connectrpc::ErrorCode;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message;
use korpis_proto::korpis::agent::v1::AgentMessage;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

async fn node(h: &Harness, id: &str) -> pb::Node {
    let got = h
        .nodes(h.root())
        .get_node(pb::GetNodeRequest { id: id.to_owned(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    got.node.as_option().unwrap().clone()
}

fn nonce(n: u8) -> [u8; 16] {
    [n; 16]
}

#[tokio::test]
async fn a_signed_hello_opens_a_session_and_the_node_is_ready() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let before = node(&h, &a.node).await;
    assert_eq!(before.status.as_known(), Some(pb::NodeStatus::NODE_STATUS_REGISTERING));
    let (_stream, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &nonce(1))).await;
    let w = welcome(&first.unwrap());
    assert_eq!(w.protocol, "korpis.agent.v1");
    assert_eq!(w.heartbeat_interval.as_option().map(|d| d.seconds), Some(30));
    let after = node(&h, &a.node).await;
    assert_eq!(after.status.as_known(), Some(pb::NodeStatus::NODE_STATUS_READY));
    assert_eq!(after.agent_version, "0.1.0-test");
    assert!(after.drivers.iter().any(|d| d.name == "oci"), "what the agent reported");
    assert!(after.capacity.as_option().is_some_and(|c| c.memory_bytes > 0));
}

#[tokio::test]
async fn a_forged_replayed_or_stale_hello_is_refused() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let now = SystemTime::now();
    let (_s, first) = TestAgent::open(&h, a.hello_at(now, &nonce(1))).await;
    first.unwrap();
    let (_s, replayed) = TestAgent::open(&h, a.hello_at(now, &nonce(1))).await;
    assert_eq!(replayed.unwrap_err().code, ErrorCode::Unauthenticated, "a nonce opens once");

    let forger = TestAgent { key: ed25519_dalek::SigningKey::from_bytes(&[9; 32]), ..a };
    let (_s, by_another) = TestAgent::open(&h, forger.hello_at(now, &nonce(2))).await;
    assert_eq!(by_another.unwrap_err().code, ErrorCode::Unauthenticated, "another key");

    let a = TestAgent { key: ed25519_dalek::SigningKey::from_bytes(&[1; 32]), ..forger };
    let stale = now - Duration::from_secs(3600);
    let (_s, old) = TestAgent::open(&h, a.hello_at(stale, &nonce(3))).await;
    let e = old.unwrap_err();
    assert_eq!(reason(&e), "request.invalid", "the one refusal an honest agent can fix");
    assert_eq!(detail(&e).field, "hello.auth.timestamp");

    let mut hello = a.hello_at(now, &nonce(4));
    if let Some(korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message::Hello(x)) =
        hello.message.as_mut()
    {
        x.protocol = "korpis.agent.v9".into();
    }
    let (_s, newer) = TestAgent::open(&h, hello).await;
    assert_eq!(reason(&newer.unwrap_err()), "protocol.unsupported");
}

#[tokio::test]
async fn a_node_that_stops_its_heartbeats_is_unreachable_after_three() {
    let h = Harness::with_heartbeat(Duration::from_millis(200)).await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (mut stream, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &nonce(1))).await;
    first.unwrap();
    for _ in 0..6 {
        stream.send(TestAgent::heartbeat()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let heard = node(&h, &a.node).await;
    assert_eq!(heard.status.as_known(), Some(pb::NodeStatus::NODE_STATUS_READY), "heard");
    drop(stream);
    // Three missed heartbeats are 600 ms; the sweep that notices runs on its own schedule, which
    // a loaded machine stretches, so this waits for it rather than for a fixed time.
    let started = std::time::Instant::now();
    let unreachable = Some(pb::NodeStatus::NODE_STATUS_UNREACHABLE);
    while node(&h, &a.node).await.status.as_known() != unreachable {
        assert!(started.elapsed() < Duration::from_secs(10), "never marked unreachable");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(started.elapsed() >= Duration::from_millis(400), "not before it missed them");
    let (_s, back) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &nonce(2))).await;
    back.unwrap();
    let returned = node(&h, &a.node).await;
    assert_eq!(returned.status.as_known(), Some(pb::NodeStatus::NODE_STATUS_READY), "back");
}

/// A Hello from `a` declaring `classes`.
fn declaring(a: &TestAgent, n: u8, classes: Vec<pb::StorageClassInfo>) -> AgentMessage {
    let mut hello = a.hello_at(SystemTime::now(), &nonce(n));
    if let Some(Message::Hello(x)) = hello.message.as_mut() {
        x.storage_classes = classes;
    }
    hello
}

#[tokio::test]
async fn a_storage_class_is_kept_as_the_helper_declared_it() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let quota = pb::StorageCapabilities {
        quota_enforced: true,
        quota_capacity: true,
        quota_inodes: true,
        fencing: pb::Fencing::FENCING_NONE.into(),
        ..Default::default()
    };
    let class = pb::StorageClassInfo {
        name: "local".into(),
        backend: pb::StorageBackend::STORAGE_BACKEND_XFS_PROJECT.into(),
        capabilities: quota.clone().into(),
        capacity_bytes: 1 << 40,
        free_bytes: 1 << 39,
        ..Default::default()
    };
    let (_s, first) = TestAgent::open(&h, declaring(&a, 1, vec![class.clone()])).await;
    first.unwrap();
    let kept = node(&h, &a.node).await.storage_classes;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].backend.as_known(), Some(pb::StorageBackend::STORAGE_BACKEND_XFS_PROJECT));
    assert_eq!(kept[0].capabilities.as_option(), Some(&quota), "what it can do, as declared");

    let unspecified = pb::StorageBackend::STORAGE_BACKEND_UNSPECIFIED.into();
    let unnamed = pb::StorageClassInfo { backend: unspecified, ..class };
    let (_s, refused) = TestAgent::open(&h, declaring(&a, 2, vec![unnamed])).await;
    assert_eq!(detail(&refused.unwrap_err()).field, "storage_classes[0].backend");
}
