// SPDX-License-Identifier: AGPL-3.0-or-later

//! The data path end to end (§3.1 of docs/02-architecture.md): the agent attaches its data
//! connection to each relay it is named, the control plane's own role or a relay running as its
//! own process, a client opens a session there with a token, and the node judges it again and
//! serves it with the handler for its action.

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

pub type Client = connectrpc::client::BidiStream<
    <connectrpc::client::HttpClient as connectrpc::client::ClientTransport>::ResponseBody,
    pb::RelayRequest,
    pb::__buffa::view::RelayResponseView<'static>,
>;

/// A session opened with `request` at `relay`, retried while the node is not yet attached, and
/// while it does not yet hold the workload: a workload declared just before is placed on the
/// node at once, and its assignment reaches the node with the next set it is sent, which under a
/// loaded machine can come after the session asks.
pub async fn opened(
    relay: &pb::RelayServiceClient<connectrpc::client::HttpClient>,
    request: &pb::RelayRequest,
) -> Client {
    loop {
        let mut client = relay.open().await.unwrap();
        client.send(request.clone()).await.unwrap();
        let first = match client.message::<pb::RelayResponse>().await {
            Ok(m) => m.map(|m| m.to_owned_message()),
            // Until the data connection is attached, the node is unavailable here.
            Err(e) if e.code == connectrpc::ErrorCode::Unavailable || not_held_yet(&e) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            Err(e) => panic!("{e:?}"),
        };
        assert!(matches!(first.and_then(|m| m.message), Some(Reply::Opened(_))), "opened first");
        return client;
    }
}

/// Whether `e` is the node's refusal of a workload it does not hold, as `korpis_agent::data`
/// words it.
fn not_held_yet(e: &connectrpc::ConnectError) -> bool {
    e.code == connectrpc::ErrorCode::FailedPrecondition
        && e.message.as_deref() == Some("this node does not hold that workload")
}

pub fn open(token: &str, action: &str, workload: &str, node: &str) -> pb::RelayRequest {
    let open = pb::RelayOpen {
        token: token.into(),
        action: action.into(),
        workload: workload.into(),
        node: node.into(),
        ..Default::default()
    };
    pb::RelayRequest { message: Some(Request::Open(Box::new(open))), ..Default::default() }
}

pub fn data(bytes: &[u8]) -> pb::RelayRequest {
    let frame = pb::SessionFrame { frame: Some(Frame::Data(bytes.to_vec())), ..Default::default() };
    pb::RelayRequest { message: Some(Request::Frame(Box::new(frame))), ..Default::default() }
}

/// A node holding one workload, serving sessions through its data connection.
pub struct Node {
    pub id: String,
    pub workload: String,
    _tasks: Vec<tokio::task::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

async fn node(cp: &ControlPlane, name: &str) -> Node {
    node_with(cp, name, Handlers::default().with(CONSOLE, Arc::new(Echo))).await
}

/// A node holding one workload, serving `handlers` through its data connection.
pub async fn node_with(cp: &ControlPlane, name: &str, handlers: Handlers) -> Node {
    let project = cp.project().await;
    let (token, ca) = cp.token(name).await;
    let dir = tempfile::tempdir().unwrap();
    let state = StateDir::new(dir.path().join("agent"));
    let join_request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    let identity = join(&state, &join_request).await.unwrap();
    let journal = JournalHandle::open(&state.root().join("journal"), Bounds::default()).unwrap();
    let (session, link) = Session::new(identity.clone(), state, Arc::new(facts), journal);
    let mut events = link.events.subscribe();
    let running = tokio::spawn(session.run());
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    // Boxed: a declaration's future would otherwise make every test's large.
    let workload = Box::pin(cp.declare(&project, "mc")).await;
    while !link.held.borrow().assignments.contains_key(&workload) {
        until(&mut events, |e| matches!(e, Event::Held { .. })).await;
    }
    let judged = Judged {
        keys: tokio::sync::watch::channel(std::sync::Arc::new(token_ring(
            &identity.keys.token_keys,
        )))
        .1,
        revoked: link.revoked.clone(),
        held: link.held.clone(),
        handlers,
    };
    let data = tokio::spawn(DataLink::new(identity.clone(), judged, link.relays.clone()).run());
    Node { id: identity.node, workload, _tasks: vec![running, data], _dir: dir }
}

/// The next frame the client is sent.
pub async fn frame(client: &mut Client) -> Frame {
    let next = tokio::time::timeout(Duration::from_secs(10), client.message::<pb::RelayResponse>());
    match next.await.unwrap().unwrap().map(|m| m.to_owned_message()).and_then(|m| m.message) {
        Some(Reply::Frame(f)) => f.frame.unwrap(),
        other => panic!("a frame, got {other:?}"),
    }
}

fn renew(token: &str) -> pb::RelayRequest {
    let frame = pb::SessionFrame { frame: Some(Frame::Renew(token.into())), ..Default::default() };
    pb::RelayRequest { message: Some(Request::Frame(Box::new(frame))), ..Default::default() }
}

pub fn end_code(f: Frame) -> String {
    let Frame::End(end) = f else { panic!("the end, got {f:?}") };
    end.code
}

#[tokio::test]
async fn a_client_reaches_a_workload_on_its_node_through_the_relay() {
    let cp = ControlPlane::start().await;
    let n = node(&cp, "fra-5").await;
    let console = cp.mint(&n.workload, &[CONSOLE]).await;
    let mut client = opened(&cp.relay(), &open(&console, CONSOLE, &n.workload, &n.id)).await;
    client.send(data(b"ping")).await.unwrap();
    assert_eq!(frame(&mut client).await, Frame::Data(b"ping".to_vec()));
    client.send(data(b"bye")).await.unwrap();
    assert_eq!(
        end_code(frame(&mut client).await),
        "bye",
        "the handler's reason reaches the client"
    );

    let exec = cp.mint(&n.workload, &["workload.exec"]).await;
    let mut unserved = cp.relay().open().await.unwrap();
    unserved.send(open(&exec, "workload.exec", &n.workload, &n.id)).await.unwrap();
    let e = unserved.message::<pb::RelayResponse>().await.unwrap_err();
    assert_eq!(e.code, connectrpc::ErrorCode::FailedPrecondition, "no handler for exec: {e:?}");
}

#[tokio::test]
async fn a_tenant_who_may_not_read_the_node_reaches_the_workload_by_its_sealed_route() {
    let cp = ControlPlane::start().await;
    let n = node(&cp, "fra-10").await;
    let (_, secret) = cp.grant_to_new_token("console", &[CONSOLE]).await;
    let minted = cp.minted(&secret, &n.workload, &[CONSOLE], 0).await;
    assert!(minted.route.starts_with("r1."), "a placed workload's token comes with its route");
    assert!(!minted.route.contains(&n.id[3..]), "and the route does not say the node");
    let mut request = open(&minted.token, CONSOLE, &n.workload, "");
    if let Some(Request::Open(o)) = request.message.as_mut() {
        o.route = minted.route.clone();
    }
    let mut client = opened(&cp.relay(), &request).await;
    client.send(data(b"ping")).await.unwrap();
    assert_eq!(frame(&mut client).await, Frame::Data(b"ping".to_vec()), "carried to the node");

    let other = cp.declare(&cp.project().await, "other").await;
    let elsewhere = cp.minted(&secret, &other, &[CONSOLE], 0).await;
    if let Some(Request::Open(o)) = request.message.as_mut() {
        o.route = elsewhere.route;
    }
    let mut wrong = cp.relay().open().await.unwrap();
    wrong.send(request).await.unwrap();
    let e = wrong.message::<pb::RelayResponse>().await.unwrap_err();
    assert_eq!(e.code, connectrpc::ErrorCode::InvalidArgument, "another workload's route: {e:?}");
}

#[tokio::test]
async fn a_session_lives_as_long_as_its_token_and_its_renewals() {
    let cp = ControlPlane::start().await;
    let n = node(&cp, "fra-6").await;
    let (_, secret) = cp.grant_to_new_token("console", &[CONSOLE]).await;
    let short = cp.mint_as(&secret, &n.workload, &[CONSOLE], 2).await;
    let mut client = opened(&cp.relay(), &open(&short, CONSOLE, &n.workload, &n.id)).await;
    client.send(renew(&cp.mint_as(&secret, &n.workload, &[CONSOLE], 4).await)).await.unwrap();
    assert!(matches!(frame(&mut client).await, Frame::Renewed(_)), "the new expiry, acknowledged");
    tokio::time::sleep(Duration::from_secs(2)).await;
    client.send(data(b"still")).await.unwrap();
    assert_eq!(frame(&mut client).await, Frame::Data(b"still".to_vec()), "alive past the first");
    assert_eq!(end_code(frame(&mut client).await), "expired", "and not past the renewal");

    let lasting = cp.mint_as(&secret, &n.workload, &[CONSOLE], 30).await;
    let mut other = opened(&cp.relay(), &open(&lasting, CONSOLE, &n.workload, &n.id)).await;
    other.send(renew("not-a-token")).await.unwrap();
    assert_eq!(end_code(frame(&mut other).await), "refused", "the relay refuses a bad renewal");
}

#[tokio::test]
async fn revoking_a_grant_ends_the_live_sessions_its_tokens_opened() {
    let cp = ControlPlane::start().await;
    let n = node(&cp, "fra-7").await;
    let (grant, secret) = cp.grant_to_new_token("console", &[CONSOLE]).await;
    let token = cp.mint_as(&secret, &n.workload, &[CONSOLE], 0).await;
    let mut client = opened(&cp.relay(), &open(&token, CONSOLE, &n.workload, &n.id)).await;
    client.send(data(b"ping")).await.unwrap();
    assert_eq!(frame(&mut client).await, Frame::Data(b"ping".to_vec()));
    cp.revoke(&grant).await;
    assert_eq!(end_code(frame(&mut client).await), "revoked", "within seconds, not at expiry");
}

#[tokio::test]
async fn a_relay_of_its_own_carries_sessions_and_ends_revoked_ones() {
    let cp = ControlPlane::start_as(false, |c| c).await;
    let relay = cp.standalone_relay("edge-1").await;
    let n = node(&cp, "fra-8").await;
    let (grant, secret) = cp.grant_to_new_token("console", &[CONSOLE]).await;
    let minted = cp.minted(&secret, &n.workload, &[CONSOLE], 0).await;
    let mut request = open(&minted.token, CONSOLE, &n.workload, "");
    if let Some(Request::Open(o)) = request.message.as_mut() {
        o.route = minted.route;
    }
    let mut client = opened(&cp.relay_at(&relay.url), &request).await;
    client.send(data(b"ping")).await.unwrap();
    assert_eq!(frame(&mut client).await, Frame::Data(b"ping".to_vec()), "routed by its feed");

    let mut direct = cp.relay().open().await.unwrap();
    direct.send(request).await.unwrap();
    // A route the control plane does not serve is answered once the request is complete.
    direct.close_send();
    let refused = direct.message::<pb::RelayResponse>().await;
    assert!(refused.is_err(), "the control plane does not serve the role it was told not to");

    cp.revoke(&grant).await;
    assert_eq!(end_code(frame(&mut client).await), "revoked", "the fed list reaches the session");
}
