// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's ingress on a real node (§3.2 of docs/07-networking.md). The helper opens ports 80
//! and 443 on the node's address and starts the real proxy, in a cgroup of its own under the
//! rule that lets its new connections leave only through a workload's link; the node says at
//! once that it serves the ingress; the agent feeds the proxy the routes of a workload's ingress
//! endpoints. Port 80 redirects a routed name to HTTPS and answers ACME's challenge path only for
//! a token it holds; port 443 terminates TLS for a name served with a certificate its
//! organization uploaded, which the node fetched over its session, and forwards the request with
//! a `Forwarded` header only the proxy wrote; it passes a TLS stream through by its server name to
//! a workload, which receives exactly what the client sent. An endpoint left to `acme` is given a
//! certificate from Pebble, which validates its HTTP-01 challenge for real, resolving the name
//! through a DNS server answering this machine's address and fetching the token from the proxy on
//! port 80, and is then served over HTTPS like the uploaded one. Retiring the workload withdraws
//! its routes. It needs root, containerd, and Pebble, and holds ports 80 and 443 of this machine's
//! address while it runs.

use crate::network::converged;
use crate::node::{Node, host_address, live};
use crate::recipes::push_web;
use crate::support::{ControlPlane, mutation};
use korpis_proto::korpis::v1 as pb;
use korpis_server::acme::AcmeConfig;
use korpis_server::testing::{ChallengeDns, Pebble, PebbleOptions, TestingError};
use std::net::SocketAddr;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Ports 80 and 443 of this machine's address, which one test's node serves at a time: every test
/// whose node serves the ingress holds them for as long as it runs, since the tests of this binary
/// run at once and only one helper can open them.
pub(crate) async fn the_ports() -> tokio::sync::MutexGuard<'static, ()> {
    static PORTS: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));
    PORTS.lock().await
}

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
pub(crate) fn client_hello(name: &str) -> Vec<u8> {
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

/// A test authority, and a certificate it issued for `name` with its key, in PEM.
fn issued(name: &str) -> (rustls::pki_types::CertificateDer<'static>, String, String) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let authority = ca.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca, ca_key);
    let key = rcgen::KeyPair::generate().unwrap();
    let leaf = rcgen::CertificateParams::new(vec![name.to_owned()]).unwrap();
    let leaf = leaf.signed_by(&key, &issuer).unwrap();
    (authority.der().clone(), leaf.pem(), key.serialize_pem())
}

/// Uploads a certificate for `name` to the root organization; its identifier, and the authority
/// a client trusts it by.
pub(crate) async fn uploaded(
    cp: &ControlPlane,
    name: &str,
) -> (String, rustls::pki_types::CertificateDer<'static>) {
    let (authority, chain, key) = issued(name);
    let req = pb::CreateCertificateRequest {
        mutation: mutation(&format!("certificate-{name}")).into(),
        organization: cp.root_org.clone(),
        chain,
        key,
        ..Default::default()
    };
    let client = pb::CertificateServiceClient::new(cp.http(), cp.config());
    let made = client.create_certificate(req).await.unwrap().into_owned();
    (made.certificate.into_option().unwrap().id, authority)
}

/// What `GET /` on `name` answers over TLS on port 443 of `at`, trusting `authority` alone, the
/// client claiming in a `Forwarded` header of its own to be someone else.
fn secure(
    at: SocketAddr,
    name: &str,
    authority: &rustls::pki_types::CertificateDer<'static>,
) -> Option<String> {
    use std::io::{Read as _, Write as _};
    let mut roots = rustls::RootCertStore::empty();
    roots.add(authority.clone()).ok()?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .ok()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let server = rustls::pki_types::ServerName::try_from(name.to_owned()).ok()?;
    let connection = rustls::ClientConnection::new(Arc::new(config), server).ok()?;
    let tcp = std::net::TcpStream::connect_timeout(&at, Duration::from_secs(2)).ok()?;
    tcp.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let mut tls = rustls::StreamOwned::new(connection, tcp);
    let request = format!(
        "GET / HTTP/1.1\r\nhost: {name}\r\nforwarded: for=6.6.6.6\r\nconnection: close\r\n\r\n"
    );
    tls.write_all(request.as_bytes()).ok()?;
    let mut got = String::new();
    let _ = tls.read_to_string(&mut got);
    (!got.is_empty()).then_some(got)
}

/// The first answer of [`secure`], within a minute: the node fetches the certificate first.
pub(crate) async fn securely(
    at: SocketAddr,
    name: &str,
    authority: &rustls::pki_types::CertificateDer<'static>,
) -> String {
    for _ in 0..120 {
        let (name, authority) = (name.to_owned(), authority.clone());
        let got = tokio::task::spawn_blocking(move || secure(at, &name, &authority));
        if let Some(answer) = got.await.unwrap() {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("port 443 never served {name} over TLS");
}

/// Pebble validating for real: resolving every name to this machine's address through the
/// challenge test server's DNS, and fetching HTTP-01 tokens from port 80. `None` where it is not
/// installed, and the `acme` endpoint's issuance is then not tested.
async fn directory() -> Option<(Pebble, ChallengeDns)> {
    let dns = match ChallengeDns::start(host_address()).await {
        Ok(dns) => dns,
        Err(e @ TestingError::NotInstalled(_)) => {
            #[expect(clippy::print_stderr, reason = "a skipped check says why")]
            {
                eprintln!("{e}: an acme endpoint's issuance not tested");
            }
            return None;
        }
        Err(e) => panic!("pebble-challtestsrv: {e}"),
    };
    let options =
        PebbleOptions { http_port: Some(80), dns: Some(dns.address), ..PebbleOptions::default() };
    let pebble = Pebble::start(options).await.expect("pebble");
    Some((pebble, dns))
}

/// Attests `domain` to the root organization, as the operator.
pub(crate) async fn attest(cp: &ControlPlane, domain: &str) {
    let attest = pb::CreateDomainRequest {
        mutation: mutation(domain).into(),
        organization: cp.root_org.clone(),
        name: domain.into(),
        verification: pb::DomainVerification::DOMAIN_VERIFICATION_OPERATOR.into(),
        ..Default::default()
    };
    pb::DomainServiceClient::new(cp.http(), cp.config()).create_domain(attest).await.unwrap();
}

/// Pebble's issuing root, which what it issues is trusted by.
pub(crate) async fn issuing_root(pebble: &Pebble) -> rustls::pki_types::CertificateDer<'static> {
    use rustls::pki_types::pem::PemObject as _;
    let ca = rustls::pki_types::CertificateDer::from_pem_file(&pebble.ca).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let http = reqwest::Client::builder().tls_backend_preconfigured(tls).build().unwrap();
    let pem = http.get(format!("{}/roots/0", pebble.management)).send().await.unwrap();
    let pem = pem.text().await.unwrap();
    rustls::pki_types::CertificateDer::from_pem_slice(pem.as_bytes()).unwrap()
}

/// The workload's endpoints: `web`, HTTP, named `web.example.com` and served with `certificate`;
/// `auto`, HTTP on the same port, named `auto.example.com`, its certificate left to `acme`; and
/// `tls`, its TLS passed through, named `tls.example.com`.
fn endpoints(certificate: &str) -> Vec<pb::EndpointSpec> {
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
    let mut web = ingress("web", pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP, 8080);
    web.tls = pb::TlsSpec {
        source: pb::TlsSource::TLS_SOURCE_PROVIDED.into(),
        secret: certificate.into(),
        ..Default::default()
    }
    .into();
    let auto = ingress("auto", pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP, 8080);
    vec![web, auto, tls]
}

/// Waits until the control plane holds the node as serving the ingress.
pub(crate) async fn serving(cp: &ControlPlane) {
    for _ in 0..100 {
        if cp.scalar::<bool>("SELECT coalesce(bool_and(ingress), false) FROM node").await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the node's ingress was not reported within ten seconds");
}

#[tokio::test]
async fn the_proxy_redirects_on_80_terminates_and_passes_tls_through_by_name_on_443() {
    if !live() {
        return;
    }
    let _ports = the_ports().await;
    let directory = directory().await;
    let acme = directory.as_ref().map_or_else(AcmeConfig::default, |(pebble, _)| pebble.config());
    let cp = ControlPlane::start_with(acme).await;
    let project = cp.project().await;
    let node = Node::join(&cp, "edge-1").await;
    let agent = node.serving_ingress().await;
    serving(&cp).await;
    attest(&cp, "example.com").await;
    let (certificate, authority) = uploaded(&cp, "web.example.com").await;
    let body = pb::IntentBody { endpoints: endpoints(&certificate), ..Default::default() };
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

    let page = securely((host, 443).into(), "web.example.com", &authority).await;
    assert!(page.starts_with("HTTP/1.1 200") && page.contains("served"), "{page}");
    let forwarded = format!("forwarded: for={host};host=web.example.com;proto=https");
    assert!(page.contains(&forwarded), "the proxy's alone: {page}");
    assert!(!page.contains("6.6.6.6"), "the client's own is removed: {page}");

    if let Some((pebble, _)) = &directory {
        let root = issuing_root(pebble).await;
        let page = securely((host, 443).into(), "auto.example.com", &root).await;
        assert!(page.starts_with("HTTP/1.1 200") && page.contains("served"), "{page}");
    }

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
