// SPDX-License-Identifier: AGPL-3.0-or-later

//! A session for a client that cannot stream both ways ("From a browser" in §3.1 of
//! docs/02-architecture.md): opened with `Watch`, the node's frames streamed back, the client's
//! sent with `Send`, numbered, delivered in order and once, and authorized by the key only the
//! watcher was given.

use crate::agent::TestAgent;
use crate::lifecycle::workload;
use crate::relay::{CONSOLE, asked, attach, data, token};
use crate::support::{Harness, reason};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::carry_up::Message as CarryUp;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::relay_response::Message as Reply;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use std::time::Duration;

fn send(opened: &pb::RelayOpened, first: u64, frames: &[&[u8]]) -> pb::SendRequest {
    pb::SendRequest {
        session: opened.session.clone(),
        send_key: opened.send_key.clone(),
        first,
        frames: frames.iter().map(|f| data(f)).collect(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_watched_session_takes_numbered_frames_in_order_and_once() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (w, _) = workload(&h).await;
    let mut attached = attach(&h, &a).await;
    let token = token(&h, &w).await;
    let relay = pb::RelayServiceClient::new(h.http.clone(), h.config(None));
    let open = pb::RelayOpen {
        token,
        action: CONSOLE.into(),
        workload: w.clone(),
        node: a.node.clone(),
        ..Default::default()
    };
    let watching = tokio::spawn({
        let relay = relay.clone();
        async move { relay.watch(open).await.unwrap() }
    });
    let asked = asked(&mut attached).await;
    let data_path = agent::DataServiceClient::new(h.http.clone(), h.config(None));
    let mut carry = data_path.carry().await.unwrap();
    let session = CarryUp::Session(asked.session.clone());
    carry.send(agent::CarryUp { message: Some(session), ..Default::default() }).await.unwrap();
    let mut watch = watching.await.unwrap();
    let first = watch.message().await.unwrap().unwrap().to_owned_message();
    let Some(Reply::Opened(opened)) = first.message else { panic!("opened first") };
    assert_eq!(opened.session, asked.session);
    assert_eq!(opened.send_key.len(), 64, "a key only the watcher holds");

    let sent = relay.send(send(&opened, 0, &[b"a", b"b"])).await.unwrap().into_owned();
    assert_eq!(sent.delivered, 2);
    // A retried call repeats a frame already delivered: it is acknowledged, not delivered again.
    let retried = relay.send(send(&opened, 1, &[b"b", b"c"])).await.unwrap().into_owned();
    assert_eq!(retried.delivered, 3);
    let mut down = Vec::new();
    for _ in 0..3 {
        let m = carry.message::<agent::CarryDown>().await.unwrap().unwrap().to_owned_message();
        down.push(m.frame.into_option().and_then(|f| f.frame));
    }
    let bytes = |b: &[u8]| Some(Frame::Data(b.to_vec()));
    assert_eq!(down, [bytes(b"a"), bytes(b"b"), bytes(b"c")], "in order, and once");

    let gap = relay.send(send(&opened, 7, &[b"x"])).await.unwrap_err();
    assert_eq!(reason(&gap), "request.invalid", "frames never sent cannot be skipped");
    let mut stolen = send(&opened, 3, &[b"x"]);
    stolen.send_key = "0".repeat(64);
    let e = relay.send(stolen).await.unwrap_err();
    assert_eq!(reason(&e), "session.not_here", "a wrong key is no session at all");

    drop(watch);
    let mut ended = None;
    for _ in 0..50 {
        match relay.send(send(&opened, 3, &[b"d"])).await {
            Err(e) => {
                ended = Some(reason(&e).clone());
                break;
            }
            Ok(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    assert_eq!(ended.as_deref(), Some("session.not_here"), "the session ends with its watch");
}
