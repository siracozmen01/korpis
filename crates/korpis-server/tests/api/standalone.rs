// SPDX-License-Identifier: AGPL-3.0-or-later

//! A relay running as its own process (§3.1 of docs/02-architecture.md), in this test's process
//! but holding nothing of the control plane's: it joins, follows its feed, admits a node's data
//! connection by the keys the feed sent and only for its own URL, and carries a session opened
//! through it. Every node is named the relay, and the control plane alongside it.

use crate::agent::{TestAgent, next_relays};
use crate::lifecycle::workload;
use crate::relay::{Attachment, asked, open, token};
use crate::support::{Harness, mutation};
use connectrpc::ErrorCode;
use connectrpc::client::ClientConfig;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::attach_down::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::carry_up::Message as CarryUp;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::relay_response::Message as Reply;
use korpis_server::standalone::{RelayDir, join, run};
use std::time::Duration;

/// Attaches with `hello` to the relay `config` reaches; the attachment, or why it was refused.
async fn attach(
    h: &Harness,
    config: ClientConfig,
    hello: agent::AttachUp,
) -> Result<Attachment, ErrorCode> {
    let client = agent::DataServiceClient::new(h.http.clone(), config);
    let mut stream = client.attach().await.map_err(|e| e.code)?;
    stream.send(hello).await.map_err(|e| e.code)?;
    let first = stream.message::<agent::AttachDown>().await.map_err(|e| e.code)?;
    let first = first.ok_or(ErrorCode::Unavailable)?.to_owned_message();
    assert!(matches!(first.message, Some(Down::Attached(_))), "{first:?}");
    Ok(stream)
}

/// A relay joined with a URL on a port it can listen on: its directory, kept while the
/// returned guard lives, its URL, and the listener.
async fn joined(h: &Harness) -> (tempfile::TempDir, RelayDir, String, tokio::net::TcpListener) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("https://localhost:{}", listener.local_addr().unwrap().port());
    let created = h
        .nodes(h.root())
        .create_relay_enrollment_token(pb::CreateRelayEnrollmentTokenRequest {
            mutation: mutation("relay-edge-1").into(),
            organization: h.root_org(),
            name: "edge-1".into(),
            url: url.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let dir = tempfile::tempdir().unwrap();
    let relay_dir = RelayDir::new(dir.path().join("relay"));
    join(&relay_dir, &h.base_url(), &created.token, &created.ca_fingerprint).await.unwrap();
    (dir, relay_dir, url, listener)
}

#[tokio::test]
async fn a_relay_of_its_own_admits_nodes_by_its_feed_and_carries_their_sessions() {
    let h = Harness::new().await;
    let (_dir, relay_dir, url, listener) = joined(&h).await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let running = run(relay_dir, listener).await.unwrap();
    let at_relay = || ClientConfig::new(url.parse().unwrap());

    let (mut session, _) =
        TestAgent::open(&h, a.hello_at(std::time::SystemTime::now(), &[1; 16])).await;
    let mut named = next_relays(&mut session).await;
    while named.urls.is_empty() {
        named = next_relays(&mut session).await;
    }
    assert_eq!((named.urls, named.control_plane), (vec![url.clone()], true), "every node is told");

    let wrong = attach(&h, at_relay(), a.data_hello(&h.base_url(), &[2; 16])).await;
    assert!(
        matches!(wrong, Err(ErrorCode::Unauthenticated | ErrorCode::Unavailable)),
        "another URL's"
    );
    let stolen = attach(&h, h.config(None), a.data_hello(&url, &[3; 16])).await;
    assert_eq!(
        stolen.err(),
        Some(ErrorCode::Unauthenticated),
        "the control plane refuses a relay's"
    );

    let mut attached = None;
    for nonce in 10..60u8 {
        match attach(&h, at_relay(), a.data_hello(&url, &[nonce; 16])).await {
            Ok(stream) => {
                attached = Some((stream, nonce));
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
    let (mut attached, nonce) = attached.expect("admitted once the feed sent the node's key");
    let replayed = attach(&h, at_relay(), a.data_hello(&url, &[nonce; 16])).await;
    assert_eq!(replayed.err(), Some(ErrorCode::Unauthenticated), "a nonce is accepted once");

    let (w, _) = workload(&h).await;
    let token = token(&h, &w).await;
    let mut client = pb::RelayServiceClient::new(h.http.clone(), at_relay()).open().await.unwrap();
    client.send(open(&token, &w, &a.node)).await.unwrap();
    let asked = asked(&mut attached).await;
    let mut carry =
        agent::DataServiceClient::new(h.http.clone(), at_relay()).carry().await.unwrap();
    carry
        .send(agent::CarryUp {
            message: Some(CarryUp::Session(asked.session.clone())),
            ..Default::default()
        })
        .await
        .unwrap();
    let opened = client.message::<pb::RelayResponse>().await.unwrap().unwrap().to_owned_message();
    let Some(Reply::Opened(opened)) = opened.message else { panic!("opened") };
    assert_eq!(opened.session, asked.session, "carried by the relay of its own");
    running.task.abort();
}
