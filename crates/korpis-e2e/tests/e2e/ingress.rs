// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's ingress on a real node (§3.2 of docs/07-networking.md). The helper opens ports 80
//! and 443 on the node's address and starts the real proxy, in a cgroup of its own under the
//! rule that lets its new connections leave only through a workload's link; the node says at
//! once that it serves the ingress; the agent feeds the proxy the routes of a workload's ingress
//! endpoints. Port 80 redirects a routed name to HTTPS and answers ACME's challenge path only for
//! a token it holds; port 443 passes a TLS stream through by its server name to the workload,
//! which receives exactly what the client sent; retiring the workload withdraws its routes.
//! Terminating TLS needs a certificate, which issuance brings with its own test. It needs root
//! and containerd, and holds ports 80 and 443 of this machine's address while it runs.

use crate::network::converged;
use crate::node::{Node, host_address, live};
use crate::recipes::push_web;
use crate::support::{ControlPlane, mutation};
use korpis_proto::korpis::v1 as pb;
use std::net::SocketAddr;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// What the node's port 80 answers a request for `path` on `name`, within two seconds.
async fn plain(at: SocketAddr, name: &str, path: &str) -> String {
    let ask = async {
        let mut s = tokio::net::TcpStream::connect(at).await.ok()?;
        let request = format!("GET {path} HTTP/1.1\r\nhost: {name}\r\nconnection: close\r\n\r\n");
        s.write_all(request.as_bytes()).await.ok()?;
        let mut got = String::new();
        s.read_to_string(&mut got).await.ok()?;
        Some(got)
    };
    tokio::time::timeout(Duration::from_secs(2), ask).await.ok().flatten().unwrap_or_default()
}

/// The first answer of port 80 for `path` on `name` that `wanted` accepts, within a minute.
async fn answered(at: SocketAddr, name: &str, path: &str, wanted: fn(&str) -> bool) -> String {
    let mut last = String::new();
    for _ in 0..300 {
        last = plain(at, name, path).await;
        if wanted(&last) {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("port 80 never answered {name}{path} as wanted; last: {last:?}");
}

/// A TLS `ClientHello` naming `name`, as any client's first bytes.
fn client_hello(name: &str) -> Vec<u8> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    let server = rustls::pki_types::ServerName::try_from(name.to_owned()).unwrap();
    let mut connection = rustls::ClientConnection::new(Arc::new(config), server).unwrap();
    let mut hello = Vec::new();
    connection.write_tls(&mut hello).unwrap();
    hello
}

/// What comes back on port 443 for `hello`: as many bytes as it has, or what arrived before the
/// proxy closed the connection or two seconds passed.
async fn passed(at: SocketAddr, hello: &[u8]) -> Vec<u8> {
    let mut back = Vec::new();
    let exchange = async {
        let mut s = tokio::net::TcpStream::connect(at).await.ok()?;
        s.write_all(hello).await.ok()?;
        let mut chunk = [0u8; 4096];
        while back.len() < hello.len() {
            let n = s.read(&mut chunk).await.ok()?;
            back.extend_from_slice(chunk.get(..n)?);
            if n == 0 {
                break;
            }
        }
        Some(())
    };
    let _ = tokio::time::timeout(Duration::from_secs(2), exchange).await;
    back
}

/// The workload's endpoints: `web`, HTTP, named `web.example.com`, and `tls`, its TLS passed
/// through, named `tls.example.com`.
fn endpoints() -> Vec<pb::EndpointSpec> {
    let ingress = |name: &str, protocol: pb::EndpointProtocol, port: u32| pb::EndpointSpec {
        name: name.into(),
        protocol: protocol.into(),
        container_port: port,
        exposure: pb::Exposure::EXPOSURE_INGRESS.into(),
        domains: vec![format!("{name}.example.com")],
        ..Default::default()
    };
    let mut tls = ingress("tls", pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP, 8443);
    tls.tls =
        pb::TlsSpec { source: pb::TlsSource::TLS_SOURCE_PASSTHROUGH.into(), ..Default::default() }
            .into();
    vec![ingress("web", pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP, 8080), tls]
}

/// Waits until the control plane holds the node as serving the ingress.
async fn serving(cp: &ControlPlane) {
    for _ in 0..100 {
        if cp.scalar::<bool>("SELECT coalesce(bool_and(ingress), false) FROM node").await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the node's ingress was not reported within ten seconds");
}

#[tokio::test]
async fn the_proxy_redirects_on_80_and_passes_tls_through_by_name_on_443() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "edge-1").await;
    let agent = node.serving_ingress().await;
    serving(&cp).await;
    let attest = pb::CreateDomainRequest {
        mutation: mutation("example.com").into(),
        organization: cp.root_org.clone(),
        name: "example.com".into(),
        verification: pb::DomainVerification::DOMAIN_VERIFICATION_OPERATOR.into(),
        ..Default::default()
    };
    pb::DomainServiceClient::new(cp.http(), cp.config()).create_domain(attest).await.unwrap();
    let body = pb::IntentBody { endpoints: endpoints(), ..Default::default() };
    let w = cp.declare_body(&project, "web", &push_web().await, body).await;
    node.guard(&w);
    converged(&cp, &w).await;

    let host = host_address();
    let redirect = |a: &str| a.starts_with("HTTP/1.1 308");
    let moved = answered((host, 80).into(), "web.example.com", "/shop?x=1", redirect).await;
    assert!(moved.contains("location: https://web.example.com/shop?x=1"), "{moved}");
    let challenge =
        plain((host, 80).into(), "web.example.com", "/.well-known/acme-challenge/t").await;
    assert!(challenge.starts_with("HTTP/1.1 404"), "a token it does not hold: {challenge}");
    let elsewhere = plain((host, 80).into(), "elsewhere.example.org", "/").await;
    assert!(elsewhere.starts_with("HTTP/1.1 404"), "a name it does not route: {elsewhere}");

    let hello = client_hello("tls.example.com");
    assert_eq!(passed((host, 443).into(), &hello).await, hello, "the workload echoed it");
    let unrouted = passed((host, 443).into(), &client_hello("other.example.org")).await;
    assert!(unrouted.is_empty(), "a name it does not route is closed");
    let chain = ["list", "chain", "inet", "korpis", "ingress_output"];
    let listed = Command::new("nft").args(chain).output().unwrap();
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(listed.contains("socket cgroupv2") && listed.contains("kw*"), "{listed}");

    cp.retire(&w).await;
    let gone = |a: &str| a.starts_with("HTTP/1.1 404");
    answered((host, 80).into(), "web.example.com", "/", gone).await;
    agent.stop().await;
}
