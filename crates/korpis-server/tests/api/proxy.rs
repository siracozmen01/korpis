// SPDX-License-Identifier: AGPL-3.0-or-later

//! A relay behind a load balancer (§3.1 of docs/02-architecture.md): connections from the
//! balancer's address open with a PROXY header, and the client it names is the one `source_cidr`
//! is judged by; a connection from that address with no header is refused.

use crate::agent::TestAgent;
use crate::lifecycle::workload;
use crate::relay::{CONSOLE, asked, open};
use crate::standalone::{admitted_at, joined};
use crate::support::{Harness, mutation};
use connectrpc::ErrorCode;
use connectrpc::client::ClientConfig;
use korpis_core::authz::{Kind, ObjectRef, Scope};
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::{principal, scope};
use korpis_server::standalone::run;
use std::net::SocketAddr;

/// A load balancer in front of `upstream`: each connection it accepts is passed on behind a
/// PROXY header naming `client`. Its port.
async fn balancer(upstream: SocketAddr, client: &'static str) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut down, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt as _;
                let mut up = tokio::net::TcpStream::connect(upstream).await.unwrap();
                let header = format!("PROXY TCP4 {client} 127.0.0.1 40000 {}\r\n", upstream.port());
                up.write_all(header.as_bytes()).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut down, &mut up).await;
            });
        }
    });
    port
}

/// A console token for `workload`, usable only from `cidrs`; the test mints it from loopback, which
/// the gateway judges by the same condition, so loopback is among them.
async fn token_from(h: &Harness, workload: &str, cidrs: &[&str]) -> String {
    let (subject, secret) = h.bare_token().await;
    let w = Id::parse(IdKind::Workload, workload).unwrap().uuid();
    let on = scope::to_pb(&Scope::Object(ObjectRef::new(Kind::Workload, w)));
    let issued = h
        .grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("console-from").into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(subject)).into(),
            actions: vec![CONSOLE.into(), "workload.read".into()],
            scope: on.clone().into(),
            conditions: pb::Conditions {
                source_cidr: cidrs.iter().map(|c| (*c).to_owned()).collect(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let minted = h
        .grants(h.config(Some(secret.expose())))
        .mint_token(pb::MintTokenRequest {
            grant: issued.grant.as_option().unwrap().id.clone(),
            actions: vec![CONSOLE.into()],
            scope: on.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    minted.token
}

#[tokio::test]
async fn behind_a_balancer_the_relay_judges_the_client_its_header_names() {
    let h = Harness::new().await;
    let (_dir, relay_dir, url, listener) = joined(&h).await;
    let relay_addr = listener.local_addr().unwrap();
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let running = run(relay_dir, listener, vec!["127.0.0.1/32".parse().unwrap()]).await.unwrap();
    let near = format!("https://localhost:{}", balancer(relay_addr, "203.0.113.7").await);
    let far = format!("https://localhost:{}", balancer(relay_addr, "198.51.100.9").await);
    let (mut attached, _) = admitted_at(&h, &a, &url, &far).await;

    let (w, _) = workload(&h).await;
    let token = token_from(&h, &w, &["127.0.0.0/8", "203.0.113.0/24"]).await;
    let at = |dial: &str| {
        pb::RelayServiceClient::new(h.http.clone(), ClientConfig::new(dial.parse().unwrap()))
    };
    let mut from_near = at(&near).open().await.unwrap();
    from_near.send(open(&token, &w, &a.node)).await.unwrap();
    let asked = asked(&mut attached).await;
    assert_eq!(asked.token, token, "the named client is inside the token's range");

    let mut from_far = at(&far).open().await.unwrap();
    from_far.send(open(&token, &w, &a.node)).await.unwrap();
    let refused = from_far.message::<pb::RelayResponse>().await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied, "outside it: {refused:?}");

    let mut bare = at(&url).open().await;
    let headerless = match &mut bare {
        Ok(stream) => stream.message::<pb::RelayResponse>().await.is_err(),
        Err(_) => true,
    };
    assert!(headerless, "the balancer's address without a header is no client");
    running.task.abort();
}
