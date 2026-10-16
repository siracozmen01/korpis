// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's ingress as its agent runs it (§3.2 of docs/07-networking.md), the test playing the
//! proxy the helper would start: the agent holds the sockets and says at once that it serves the
//! ingress, hands them to the proxy that connects, sends it the routes of its ingress endpoints as
//! they change, and has it started again when it goes away.

use crate::session::until;
use crate::support::{ControlPlane, facts, mutation, push_paper};
use buffa::Message as _;
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::network::Pools;
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::ingress::v1 as ingress;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use std::io::Read as _;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// What the proxy receives on connecting: the first message and the sockets with it.
fn connect(control: &PathBuf) -> (UnixStream, ingress::Listeners, Vec<OwnedFd>) {
    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, recvmsg};
    let mut s = UnixStream::connect(control).unwrap();
    let mut head = [0u8; 4];
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(8))];
    let mut cmsg = RecvAncillaryBuffer::new(&mut space);
    let got =
        recvmsg(&s, &mut [std::io::IoSliceMut::new(&mut head)], &mut cmsg, RecvFlags::empty());
    let got = got.unwrap().bytes;
    let mut fds = Vec::new();
    for m in cmsg.drain() {
        if let RecvAncillaryMessage::ScmRights(f) = m {
            fds.extend(f);
        }
    }
    s.read_exact(&mut head[got..]).unwrap();
    let mut body = vec![0u8; usize::try_from(u32::from_be_bytes(head)).unwrap()];
    s.read_exact(&mut body).unwrap();
    (s, ingress::Listeners::decode_from_slice(&body).unwrap(), fds)
}

fn next_config(s: &mut UnixStream) -> ingress::IngressConfig {
    let mut head = [0u8; 4];
    s.read_exact(&mut head).unwrap();
    let mut body = vec![0u8; usize::try_from(u32::from_be_bytes(head)).unwrap()];
    s.read_exact(&mut body).unwrap();
    ingress::IngressConfig::decode_from_slice(&body).unwrap()
}

/// The first configuration on `s` with a route in it.
fn routed(mut s: UnixStream) -> (UnixStream, ingress::IngressConfig) {
    loop {
        let c = next_config(&mut s);
        if !c.routes.is_empty() {
            return (s, c);
        }
    }
}

/// The control socket, once the agent has had the proxy started `n` times.
async fn started(host: &InMemoryHost, n: u32) -> PathBuf {
    for _ in 0..150 {
        if let (Some(control), starts) = host.ingress()
            && starts >= n
        {
            return control;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the proxy was not started {n} times: {:?}", host.ingress());
}

/// Waits until the control plane holds the node as serving the ingress, which the agent reports
/// as soon as it holds the sockets rather than at its next heartbeat, thirty seconds away.
async fn advertised_at_once(cp: &ControlPlane) {
    for _ in 0..50 {
        if cp.scalar::<bool>("SELECT coalesce(bool_and(ingress), false) FROM node").await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the node's ingress was not reported within five seconds");
}

async fn declare_web(cp: &ControlPlane, project: &str) {
    let web = pb::EndpointSpec {
        name: "web".into(),
        protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP.into(),
        container_port: 8080,
        exposure: pb::Exposure::EXPOSURE_INGRESS.into(),
        domains: vec!["shop.example.com".into()],
        ..Default::default()
    };
    let recipe =
        pb::RecipeRef { name: push_paper().await, version: "1.21.4".into(), ..Default::default() };
    let body = pb::IntentBody { recipe: recipe.into(), endpoints: vec![web], ..Default::default() };
    cp.workloads()
        .declare_intent(pb::DeclareIntentRequest {
            mutation: mutation("declare-shop").into(),
            target: Some(Target::Create(Box::new(pb::NewWorkload {
                project: project.to_owned(),
                name: "shop".into(),
                ..Default::default()
            }))),
            body: body.into(),
            ..Default::default()
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn the_agent_hands_its_proxy_the_sockets_and_the_routes_and_restarts_it() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (token, ca) = cp.token("edge-1").await;
    let state = StateDir::new(dir.path().join("edge-1"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let serving = Arc::new(AtomicBool::new(false));
    let options = Options {
        period: Duration::from_millis(200),
        network: Some(Pools { ipv4: "10.47.0.0/16".parse().unwrap(), ipv6: None }),
        ingress: Some(Arc::clone(&serving)),
        ..Options::default()
    };
    let host = InMemoryHost::new();
    // As `Advertise` does: the node says it serves the ingress while the agent holds its sockets.
    let advertised = Arc::clone(&serving);
    let facts = Arc::new(move || {
        let mut f = facts();
        f.advertised.ingress = advertised.load(Ordering::Relaxed);
        f
    });
    let agent = Agent::start(&state, InMemory::new(), host.clone(), facts, options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;

    let control = started(&host, 1).await;
    let (mut proxy, listeners, fds) =
        tokio::task::spawn_blocking(move || connect(&control)).await.unwrap();
    assert_eq!((listeners.http, listeners.https, fds.len()), (1, 1, 2), "port 80, then 443");
    assert!(serving.load(Ordering::Relaxed), "the node says it serves the ingress");
    let (proxy, first) = tokio::task::spawn_blocking(move || {
        let c = next_config(&mut proxy);
        (proxy, c)
    })
    .await
    .unwrap();
    assert!(first.routes.is_empty());

    cp.measured().await;
    advertised_at_once(&cp).await;
    let attest = pb::CreateDomainRequest {
        mutation: mutation("domain").into(),
        organization: cp.root_org.clone(),
        name: "example.com".into(),
        verification: pb::DomainVerification::DOMAIN_VERIFICATION_OPERATOR.into(),
        ..Default::default()
    };
    cp.client(pb::DomainServiceClient::new).create_domain(attest).await.unwrap();
    let project = cp.project().await;
    declare_web(&cp, &project).await;
    let (proxy, routed) = tokio::task::spawn_blocking(move || routed(proxy)).await.unwrap();
    let route = &routed.routes[0];
    assert_eq!(route.names, ["shop.example.com"]);
    assert!(route.backend.starts_with("10.47.") && route.backend.ends_with(":8080"), "{route:?}");
    assert_eq!(route.mode.as_known(), Some(ingress::RouteMode::ROUTE_MODE_HTTP));

    drop(proxy);
    started(&host, 2).await;
    agent.stop().await;
}
