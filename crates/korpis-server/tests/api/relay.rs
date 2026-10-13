// SPDX-License-Identifier: AGPL-3.0-or-later

//! The relay role (§3.1 of docs/02-architecture.md), with the node played by hand: a node
//! attaches its data connection, a client opens a session with a token, the relay verifies it and
//! asks the node, and the node's carry joins the two. What the relay refuses, it refuses before
//! the node hears of it; what the node refuses reaches the client as the node's reason.

use crate::agent::TestAgent;
use crate::lifecycle::workload;
use crate::support::Harness;
use connectrpc::ErrorCode;
use connectrpc::client::{BidiStream, HttpClient};
use korpis_core::authz::{Kind, ObjectRef, Scope};
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::attach_down::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::attach_up::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::carry_up::Message as CarryUp;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::relay_request::Message as Request;
use korpis_proto::korpis::v1::__buffa::oneof::relay_response::Message as Reply;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use korpis_wire::scope;
use std::time::Duration;

const CONSOLE: &str = "workload.console.write";

type Attachment = BidiStream<
    <HttpClient as connectrpc::client::ClientTransport>::ResponseBody,
    agent::AttachUp,
    agent::__buffa::view::AttachDownView<'static>,
>;

async fn attach(h: &Harness, a: &TestAgent) -> Attachment {
    let client = agent::DataServiceClient::new(h.http.clone(), h.config(None));
    let mut stream = client.attach().await.unwrap();
    stream.send(a.data_hello(&[7; 16])).await.unwrap();
    let first = stream.message::<agent::AttachDown>().await.unwrap().unwrap().to_owned_message();
    assert!(matches!(first.message, Some(Down::Attached(_))), "{first:?}");
    stream
}

async fn asked(stream: &mut Attachment) -> agent::OpenSession {
    let next = tokio::time::timeout(Duration::from_secs(5), stream.message::<agent::AttachDown>());
    let m = next.await.unwrap().unwrap().unwrap().to_owned_message();
    let Some(Down::Open(open)) = m.message else { panic!("an open, got {m:?}") };
    *open
}

async fn token(h: &Harness, workload: &str) -> String {
    let w = Id::parse(IdKind::Workload, workload).unwrap().uuid();
    let on = scope::to_pb(&Scope::Object(ObjectRef::new(Kind::Workload, w)));
    let minted = h
        .grants(h.root())
        .mint_token(pb::MintTokenRequest {
            grant: h.root_grant(),
            actions: vec![CONSOLE.into()],
            scope: on.into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    minted.token
}

fn open(token: &str, workload: &str, node: &str) -> pb::RelayRequest {
    let open = pb::RelayOpen {
        token: token.into(),
        action: CONSOLE.into(),
        workload: workload.into(),
        node: node.into(),
        ..Default::default()
    };
    pb::RelayRequest { message: Some(Request::Open(Box::new(open))), ..Default::default() }
}

fn data(bytes: &[u8]) -> pb::SessionFrame {
    pb::SessionFrame { frame: Some(Frame::Data(bytes.to_vec())), ..Default::default() }
}

#[tokio::test]
async fn a_session_is_verified_asked_of_the_node_and_carried_both_ways() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (w, _) = workload(&h).await;
    let mut attached = attach(&h, &a).await;
    let token = token(&h, &w).await;

    let relay = pb::RelayServiceClient::new(h.http.clone(), h.config(None));
    let mut client = relay.open().await.unwrap();
    client.send(open(&token, &w, &a.node)).await.unwrap();
    let asked = asked(&mut attached).await;
    assert_eq!((asked.workload.as_str(), asked.action.as_str()), (w.as_str(), CONSOLE));
    assert_eq!(asked.token, token, "the node judges the same token again");

    let data_path = agent::DataServiceClient::new(h.http.clone(), h.config(None));
    let mut carry = data_path.carry().await.unwrap();
    let first = agent::CarryUp {
        message: Some(CarryUp::Session(asked.session.clone())),
        ..Default::default()
    };
    carry.send(first).await.unwrap();
    let up = agent::CarryUp {
        message: Some(CarryUp::Frame(Box::new(data(b"hello")))),
        ..Default::default()
    };
    carry.send(up).await.unwrap();

    let reply = |m: pb::RelayResponse| m.message.unwrap();
    let opened = client.message::<pb::RelayResponse>().await.unwrap().unwrap().to_owned_message();
    let Reply::Opened(opened) = reply(opened) else { panic!("opened first") };
    assert_eq!(opened.session, asked.session);
    assert!(opened.expires_at.as_option().is_some());
    let frame = client.message::<pb::RelayResponse>().await.unwrap().unwrap().to_owned_message();
    let Reply::Frame(frame) = reply(frame) else { panic!("a frame") };
    assert_eq!(frame.frame, Some(Frame::Data(b"hello".to_vec())));

    let back = pb::RelayRequest {
        message: Some(Request::Frame(Box::new(data(b"back")))),
        ..Default::default()
    };
    client.send(back).await.unwrap();
    let down = carry.message::<agent::CarryDown>().await.unwrap().unwrap().to_owned_message();
    assert_eq!(down.frame.into_option().and_then(|f| f.frame), Some(Frame::Data(b"back".to_vec())));

    let again =
        agent::CarryUp { message: Some(CarryUp::Session(asked.session)), ..Default::default() };
    let mut twice = data_path.carry().await.unwrap();
    twice.send(again).await.unwrap();
    assert!(twice.message::<agent::CarryDown>().await.is_err(), "a session is carried once");
}

#[tokio::test]
async fn the_relay_and_the_node_each_refuse_what_they_must() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let elsewhere = TestAgent::enroll(&h, "fra-4", 2).await;
    let (w, _) = workload(&h).await;
    let mut attached = attach(&h, &a).await;
    let token = token(&h, &w).await;
    let relay = pb::RelayServiceClient::new(h.http.clone(), h.config(None));
    let refusal = |request: pb::RelayRequest| {
        let relay = relay.clone();
        async move {
            let mut client = relay.open().await.unwrap();
            client.send(request).await.unwrap();
            client.message::<pb::RelayResponse>().await.unwrap_err().code
        }
    };

    assert_eq!(refusal(open("not-a-token", &w, &a.node)).await, ErrorCode::PermissionDenied);
    assert_eq!(
        refusal(open(&token, &w, &elsewhere.node)).await,
        ErrorCode::Unavailable,
        "not attached"
    );

    let refused = tokio::spawn(refusal(open(&token, &w, &a.node)));
    let asked = asked(&mut attached).await;
    let no = agent::OpenRefused {
        session: asked.session,
        code: "not_here".into(),
        message: "this node does not hold that workload".into(),
        ..Default::default()
    };
    attached
        .send(agent::AttachUp { message: Some(Up::Refused(Box::new(no))), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(refused.await.unwrap(), ErrorCode::FailedPrecondition, "the node's reason");
}
