// SPDX-License-Identifier: AGPL-3.0-or-later

//! The ingress's features of Phase 2 on a real node (§3.2 of docs/07-networking.md, trace 51 of
//! docs/23-walkthroughs.md), each declared as a customer declares it and served by the node's
//! proxy from the route its agent built: an endpoint that requires a client certificate serves a
//! client its own authority issued and tells the workload which one in `Client-Cert`, and gives
//! a client without one no session; a browser's HTTP/2 connection opens a WebSocket to a workload
//! that speaks HTTP/1.1; and a workload whose TLS is passed through, having declared the PROXY
//! protocol, is told who connected before the client's first byte. And a name of an `acme`
//! endpoint whose node the directory cannot reach on port 80 is proven by TLS-ALPN-01 on 443, by
//! the node's proxy, on the attempt after HTTP-01 failed, and kept that way. It needs root and
//! containerd, Pebble for the last, and holds ports 80 and 443 of this machine's address while it
//! runs.

use crate::ingress::{attest, client_hello, issuing_root, securely, serving, the_ports, uploaded};
use crate::network::converged;
use crate::node::{Node, host_address, live};
use crate::recipes::push_web;
use crate::support::ControlPlane;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty};
use hyper_util::rt::{TokioExecutor, TokioIo};
use korpis_proto::korpis::v1 as pb;
use korpis_server::testing::{ChallengeDns, Pebble, PebbleOptions, TestingError};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

type Identity = (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>);

/// An authority of the customer's own, in PEM, and a device's certificate it issued.
fn devices() -> (String, Identity) {
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let authority = ca.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca, ca_key);
    let mut device = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    device.distinguished_name.push(rcgen::DnType::CommonName, "device-7");
    device.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = device.signed_by(&key, &issuer).unwrap();
    let key = PrivateKeyDer::try_from(key.serialize_der()).unwrap();
    (authority.pem(), (vec![cert.der().clone()], key))
}

/// `app` and `api` over HTTP on 8080 with the certificates `pages_crt` and `api_crt`, `api`
/// requiring a client certificate from `authority`; and `relay`, its TLS passed through to 8443,
/// told its client in a PROXY header.
fn endpoints((pages_crt, api_crt): (&str, &str), authority: &str) -> Vec<pb::EndpointSpec> {
    let ingress = |name: &str, protocol: pb::EndpointProtocol, port: u32| pb::EndpointSpec {
        name: name.into(),
        protocol: protocol.into(),
        container_port: port,
        exposure: pb::Exposure::EXPOSURE_INGRESS.into(),
        domains: vec![format!("{name}.example.com")],
        ..Default::default()
    };
    let provided = |secret: &str| pb::TlsSpec {
        source: pb::TlsSource::TLS_SOURCE_PROVIDED.into(),
        secret: secret.into(),
        ..Default::default()
    };
    let mut app = ingress("app", pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP, 8080);
    app.tls = provided(pages_crt).into();
    let mut api = ingress("api", pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP, 8080);
    api.tls = pb::TlsSpec {
        client_certificates: pb::ClientCertificates::CLIENT_CERTIFICATES_REQUIRED.into(),
        client_ca: authority.into(),
        ..provided(api_crt)
    }
    .into();
    let mut relay = ingress("relay", pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP, 8443);
    relay.tls =
        pb::TlsSpec { source: pb::TlsSource::TLS_SOURCE_PASSTHROUGH.into(), ..Default::default() }
            .into();
    relay.client_address = pb::ClientAddress::CLIENT_ADDRESS_PROXY_PROTOCOL.into();
    vec![app, api, relay]
}

/// A TLS connection to `name` on `at`, trusting `authority`, offering `alpn`, presenting
/// `identity` when asked for a certificate.
async fn tls(
    at: SocketAddr,
    (name, alpn): (&str, &[u8]),
    authority: &CertificateDer<'static>,
    identity: Option<Identity>,
) -> std::io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(authority.clone()).map_err(std::io::Error::other)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(std::io::Error::other)?
        .with_root_certificates(roots);
    let mut config = match identity {
        Some((chain, key)) => {
            builder.with_client_auth_cert(chain, key).map_err(std::io::Error::other)?
        }
        None => builder.with_no_client_auth(),
    };
    config.alpn_protocols = vec![alpn.to_vec()];
    let server = ServerName::try_from(name.to_owned()).map_err(std::io::Error::other)?;
    let tcp = TcpStream::connect(at).await?;
    tokio_rustls::TlsConnector::from(Arc::new(config)).connect(server, tcp).await
}

/// What `GET /` on `api.example.com` answers, as `identity`, claiming a certificate of its own
/// in `Client-Cert`; none where no session was made.
async fn api(
    at: SocketAddr,
    authority: &CertificateDer<'static>,
    identity: Option<Identity>,
) -> Option<String> {
    let mut s = tls(at, ("api.example.com", b"http/1.1"), authority, identity).await.ok()?;
    let request = "GET / HTTP/1.1\r\nhost: api.example.com\r\nclient-cert: :Zm9yZ2Vk:\r\n\
                   connection: close\r\n\r\n";
    s.write_all(request.as_bytes()).await.ok()?;
    let mut got = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(2), s.read_to_end(&mut got)).await;
    (!got.is_empty()).then(|| String::from_utf8_lossy(&got).into_owned())
}

/// A WebSocket opened on an HTTP/2 connection to `app.example.com`; what it echoes of `said`.
async fn websocket_h2(at: SocketAddr, authority: &CertificateDer<'static>, said: &[u8]) -> Vec<u8> {
    let s = tls(at, ("app.example.com", b"h2"), authority, None).await.unwrap();
    let (mut send, conn) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(s)).await.unwrap();
    tokio::spawn(conn);
    let page = send.send_request(get("https://app.example.com/")).await.unwrap();
    let page = page.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&page).starts_with("served"), "a page first");
    let mut open = get("https://app.example.com/chat");
    *open.method_mut() = http::Method::CONNECT;
    open.headers_mut().insert("sec-websocket-version", "13".parse().unwrap());
    open.extensions_mut().insert(hyper::ext::Protocol::from_static("websocket"));
    let mut opened = send.send_request(open).await.unwrap();
    assert_eq!(opened.status(), http::StatusCode::OK, "HTTP/2's word for 101");
    let mut stream = TokioIo::new(hyper::upgrade::on(&mut opened).await.unwrap());
    stream.write_all(said).await.unwrap();
    let mut back = vec![0u8; said.len()];
    stream.read_exact(&mut back).await.unwrap();
    back
}

fn get(uri: &str) -> http::Request<Empty<Bytes>> {
    http::Request::get(uri).body(Empty::new()).unwrap()
}

/// What `relay` echoes of a connection to `at`: where the connection was made from, the
/// addresses its PROXY header names, read by `ppp`, and whether the client's first bytes followed
/// it unchanged.
async fn relayed(at: SocketAddr) -> (SocketAddr, ppp::v2::Addresses, bool) {
    let hello = client_hello("relay.example.com");
    let mut s = TcpStream::connect(at).await.unwrap();
    let from = s.local_addr().unwrap();
    s.write_all(&hello).await.unwrap();
    let mut back = vec![0u8; 16];
    s.read_exact(&mut back).await.unwrap();
    let length = usize::from(u16::from_be_bytes([back[14], back[15]]));
    back.resize(16 + length + hello.len(), 0);
    s.read_exact(&mut back[16..]).await.unwrap();
    let header = ppp::v2::Header::try_from(&back[..16 + length]).unwrap();
    assert_eq!(header.command, ppp::v2::Command::Proxy);
    (from, header.addresses, back[16 + length..] == hello[..])
}

#[tokio::test]
async fn a_node_asks_for_client_certificates_opens_websockets_on_http2_and_writes_proxy_headers() {
    if !live() {
        return;
    }
    let _ports = the_ports().await;
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let node = Node::join(&cp, "edge-2").await;
    let agent = node.serving_ingress().await;
    serving(&cp).await;
    attest(&cp, "example.com").await;
    let (pages_crt, pages_root) = uploaded(&cp, "app.example.com").await;
    let (api_crt, locked_root) = uploaded(&cp, "api.example.com").await;
    let (devices_authority, device) = devices();
    let body = pb::IntentBody {
        endpoints: endpoints((&pages_crt, &api_crt), &devices_authority),
        ..Default::default()
    };
    let w = cp.declare_body(&project, "ledgerline", &push_web().await, body).await;
    node.guard(&w);
    converged(&cp, &w).await;
    let at: SocketAddr = (host_address(), 443).into();

    let mut page = None;
    for _ in 0..120 {
        let device = (device.0.clone(), device.1.clone_key());
        page = api(at, &locked_root, Some(device)).await;
        if page.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let page = page.expect("the node serves api.example.com to its device");
    let told = format!("client-cert: :{}:", STANDARD.encode(device.0[0].as_ref()));
    assert!(page.starts_with("HTTP/1.1 200") && page.contains(&told), "{page}");
    assert!(!page.contains("Zm9yZ2Vk"), "the client's own claim is removed: {page}");
    assert!(api(at, &locked_root, None).await.is_none(), "no certificate, no session");

    let echoed = websocket_h2(at, &pages_root, b"hello over h2").await;
    assert_eq!(echoed, b"hello over h2", "joined to the workload byte for byte");

    let (from, addresses, unchanged) = relayed(at).await;
    assert_eq!(addresses, (from, at).into(), "the client, and the address it reached");
    assert!(unchanged, "then the client's first bytes, as it sent them");

    cp.retire(&w).await;
    agent.stop().await;
}

/// Pebble validating for real, resolving every name to this machine's address, with nothing
/// listening on the port it asks HTTP-01 at and TLS-ALPN-01 asked on 443, as a directory finds a
/// node whose port 80 a provider blocks. `None` where it is not installed.
async fn port_80_closed() -> Option<(Pebble, ChallengeDns)> {
    let dns = match ChallengeDns::start(host_address()).await {
        Ok(dns) => dns,
        Err(e @ TestingError::NotInstalled(_)) => {
            #[expect(clippy::print_stderr, reason = "a skipped check says why")]
            {
                eprintln!("{e}: TLS-ALPN-01 at a node's ingress not tested");
            }
            return None;
        }
        Err(e) => panic!("pebble-challtestsrv: {e}"),
    };
    let closed = std::net::TcpListener::bind((host_address(), 0)).ok()?.local_addr().ok()?.port();
    let options = PebbleOptions {
        http_port: Some(closed),
        tls_port: Some(443),
        dns: Some(dns.address),
        ..PebbleOptions::default()
    };
    Some((Pebble::start(options).await.expect("pebble"), dns))
}

#[tokio::test]
async fn a_name_whose_node_a_directory_cannot_reach_on_80_is_proven_by_tls_alpn_on_443() {
    if !live() {
        return;
    }
    let _ports = the_ports().await;
    let Some((pebble, _dns)) = port_80_closed().await else { return };
    let cp = ControlPlane::start_with(pebble.config()).await;
    let project = cp.project().await;
    let node = Node::join(&cp, "edge-3").await;
    let agent = node.serving_ingress().await;
    serving(&cp).await;
    attest(&cp, "example.com").await;
    let late = pb::EndpointSpec {
        name: "late".into(),
        protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP.into(),
        container_port: 8080,
        exposure: pb::Exposure::EXPOSURE_INGRESS.into(),
        domains: vec!["late.example.com".into()],
        ..Default::default()
    };
    let body = pb::IntentBody { endpoints: vec![late], ..Default::default() };
    let w = cp.declare_body(&project, "late", &push_web().await, body).await;
    node.guard(&w);
    converged(&cp, &w).await;

    // HTTP-01 first, for a certificate never issued, which nothing answers.
    for _ in 0..360 {
        if cp.scalar::<i32>("SELECT attempts FROM certificate").await >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(cp.scalar::<i32>("SELECT attempts FROM certificate").await, 1, "one failed");
    // The next attempt, without waiting out the back-off, is made the other way.
    let pool = cp.api.app().store().pool();
    sqlx::query("UPDATE certificate SET next_attempt_at = now()").execute(pool).await.unwrap();
    sqlx::query("SELECT pg_notify('korpis_certificate', '')").execute(pool).await.unwrap();
    let root = issuing_root(&pebble).await;
    let page = securely((host_address(), 443).into(), "late.example.com", &root).await;
    assert!(page.starts_with("HTTP/1.1 200") && page.contains("served"), "{page}");
    let way: String = cp.scalar("SELECT node_proof FROM certificate").await;
    assert_eq!(way, "tls-alpn-01", "kept, so its renewals try it first");

    cp.retire(&w).await;
    agent.stop().await;
}
