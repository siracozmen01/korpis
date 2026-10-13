// SPDX-License-Identifier: AGPL-3.0-or-later

//! The data path end to end (§3.1 of docs/02-architecture.md): the agent attaches its data
//! connection to the control plane's relay role, a client opens a session there with a token,
//! and the node judges it again and serves it with the handler for its action.

use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::data::handler::{Ended, Handler, Handlers, Io, Opened, Serving};
use korpis_agent::data::{DataLink, Judged};
use korpis_agent::join::{Join, join};
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::revoked::token_ring;
use korpis_agent::session::{Event, Session};
use korpis_agent::state::StateDir;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::relay_request::Message as Request;
use korpis_proto::korpis::v1::__buffa::oneof::relay_response::Message as Reply;
use korpis_proto::korpis::v1::__buffa::oneof::session_frame::Frame;
use std::sync::Arc;
use std::time::Duration;

const CONSOLE: &str = "workload.console.write";

/// Sends back what it is sent, and ends the session with `bye` when told "bye".
struct Echo;

async fn echo(opened: Opened, mut io: Io) -> Result<(), Ended> {
    assert_eq!(opened.action, CONSOLE);
    while let Some(bytes) = io.incoming.recv().await {
        if bytes == b"bye" {
            return Err(Ended { code: "bye".into(), message: "as asked".into() });
        }
        let _ = io.outgoing.send(bytes).await;
    }
    Ok(())
}

impl Handler for Echo {
    fn serve(&self, opened: Opened, io: Io) -> Serving {
        Box::pin(echo(opened, io))
    }
}

type Client = connectrpc::client::BidiStream<
    <connectrpc::client::HttpClient as connectrpc::client::ClientTransport>::ResponseBody,
    pb::RelayRequest,
    pb::__buffa::view::RelayResponseView<'static>,
>;

/// A session opened with `request`, retried while the node is not yet attached.
async fn opened(cp: &ControlPlane, request: &pb::RelayRequest) -> Client {
    loop {
        let mut client = cp.relay().open().await.unwrap();
        client.send(request.clone()).await.unwrap();
        let first = match client.message::<pb::RelayResponse>().await {
            Ok(m) => m.map(|m| m.to_owned_message()),
            // Until the data connection is attached, the node is unavailable here.
            Err(e) if e.code == connectrpc::ErrorCode::Unavailable => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            Err(e) => panic!("{e:?}"),
        };
        assert!(matches!(first.and_then(|m| m.message), Some(Reply::Opened(_))), "opened first");
        return client;
    }
}

fn open(token: &str, action: &str, workload: &str, node: &str) -> pb::RelayRequest {
    let open = pb::RelayOpen {
        token: token.into(),
        action: action.into(),
        workload: workload.into(),
        node: node.into(),
        ..Default::default()
    };
    pb::RelayRequest { message: Some(Request::Open(Box::new(open))), ..Default::default() }
}

fn data(bytes: &[u8]) -> pb::RelayRequest {
    let frame = pb::SessionFrame { frame: Some(Frame::Data(bytes.to_vec())), ..Default::default() };
    pb::RelayRequest { message: Some(Request::Frame(Box::new(frame))), ..Default::default() }
}

#[tokio::test]
async fn a_client_reaches_a_workload_on_its_node_through_the_relay() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let (token, ca) = cp.token("fra-5").await;
    let dir = tempfile::tempdir().unwrap();
    let state = StateDir::new(dir.path().join("agent"));
    let join_request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    let identity = join(&state, &join_request).await.unwrap();
    let journal = JournalHandle::open(&state.root().join("journal"), Bounds::default()).unwrap();
    let (session, link) = Session::new(identity.clone(), state, Arc::new(facts), journal);
    let mut events = link.events.subscribe();
    let _running = tokio::spawn(session.run());
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    let workload = cp.declare(&project, "mc").await;
    while !link.held.borrow().assignments.contains_key(&workload) {
        until(&mut events, |e| matches!(e, Event::Held { .. })).await;
    }
    let judged = Judged {
        keys: token_ring(&identity.keys.token_keys),
        revoked: link.revoked.clone(),
        held: link.held.clone(),
        handlers: Handlers::default().with(CONSOLE, Arc::new(Echo)),
    };
    let _data = tokio::spawn(DataLink::new(identity.clone(), judged, link.relays.clone()).run());

    let console = cp.mint(&workload, &[CONSOLE]).await;
    let mut client = opened(&cp, &open(&console, CONSOLE, &workload, &identity.node)).await;
    client.send(data(b"ping")).await.unwrap();
    let frame = |m: Option<pb::RelayResponse>| match m.and_then(|m| m.message) {
        Some(Reply::Frame(f)) => f.frame,
        other => panic!("a frame, got {other:?}"),
    };
    let echoed = client.message::<pb::RelayResponse>().await.unwrap().map(|m| m.to_owned_message());
    assert_eq!(frame(echoed), Some(Frame::Data(b"ping".to_vec())));
    client.send(data(b"bye")).await.unwrap();
    let ended = client.message::<pb::RelayResponse>().await.unwrap().map(|m| m.to_owned_message());
    let Some(Frame::End(end)) = frame(ended) else { panic!("the end") };
    assert_eq!(end.code, "bye", "the handler's reason reaches the client");

    let exec = cp.mint(&workload, &["workload.exec"]).await;
    let mut unserved = cp.relay().open().await.unwrap();
    unserved.send(open(&exec, "workload.exec", &workload, &identity.node)).await.unwrap();
    let e = unserved.message::<pb::RelayResponse>().await.unwrap_err();
    assert_eq!(e.code, connectrpc::ErrorCode::FailedPrecondition, "no handler for exec: {e:?}");
}
