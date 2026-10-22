// SPDX-License-Identifier: AGPL-3.0-or-later

//! Client certificates (§3.2 of docs/07-networking.md): a route that asks for one is accepted
//! with its own TLS configuration, verified against its own authorities, and its workload is told
//! the certificate in `Client-Cert` (RFC 9440), which no client can say for itself; and a
//! connection made for a name that asks for nothing reaches no name that requires one, though
//! one certificate serves both and HTTP/2 would reuse the connection (Finding 137 of
//! docs/23-walkthroughs.md).

use crate::support::{Identity, Pki, Proxy, get, route, tls_as, workload};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::StatusCode;
use http_body_util::BodyExt as _;
use hyper_util::rt::{TokioExecutor, TokioIo};
use korpis_proto::korpis::ingress::v1 as pb;
use std::io;
use std::net::SocketAddr;
use tokio::net::TcpStream;

use pb::ClientCertificates as C;

/// A route for `name` to `backend` with the certificate `crt_<name>`, asking clients for a
/// certificate as `asked`, from the authority in `ca`.
fn asking(name: &str, backend: SocketAddr, asked: C, ca: &str) -> pb::Route {
    let mut r = route(&[name], backend, pb::RouteMode::ROUTE_MODE_HTTP, &format!("crt_{name}"));
    (r.client_certificates, r.client_ca) = (asked.into(), ca.into());
    r
}

/// A GET naming `host` on a connection asking for `sni`, over HTTP/2 when `h2`, presenting
/// `identity` if asked for a certificate, and claiming a certificate in `Client-Cert` itself;
/// the response's status and body, or why there was none.
async fn ask(
    pki: &Pki,
    at: SocketAddr,
    (sni, host, h2): (&str, &str, bool),
    identity: Option<Identity>,
) -> io::Result<(StatusCode, String)> {
    let alpn: &[&[u8]] = if h2 { &[b"h2"] } else { &[b"http/1.1"] };
    let tls = tls_as(pki, TcpStream::connect(at).await?, (sni, alpn), identity).await?;
    let mut req = get(&format!("https://{host}/"), host);
    req.headers_mut().insert("client-cert", ":Zm9yZ2Vk:".parse().unwrap());
    let response = if h2 {
        let (mut send, conn) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
                .await
                .map_err(io::Error::other)?;
        tokio::spawn(conn);
        send.send_request(req).await.map_err(io::Error::other)?
    } else {
        let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(io::Error::other)?;
        tokio::spawn(conn);
        send.send_request(req).await.map_err(io::Error::other)?
    };
    let status = response.status();
    let body = response.into_body().collect().await.map_err(io::Error::other)?.to_bytes();
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

fn copy(identity: &Identity) -> Identity {
    (identity.0.clone(), identity.1.clone_key())
}

/// What the workload is told of `identity`'s certificate.
fn told(identity: &Identity) -> String {
    format!("client-cert=:{}:\n", STANDARD.encode(identity.0[0].as_ref()))
}

#[tokio::test]
async fn a_required_endpoint_serves_only_a_client_its_authorities_issued_and_says_which() {
    let (pki, clients, strangers) = (Pki::new(), Pki::new(), Pki::new());
    let api = workload("api", None).await;
    let name = "api.example.test";
    let proxy = Proxy::start(&pb::IngressConfig {
        routes: vec![asking(name, api, C::CLIENT_CERTIFICATES_REQUIRED, &clients.authority())],
        certificates: vec![pki.certificate("crt_api.example.test", &[name])],
        ..Default::default()
    });
    let device = clients.client("device-7");
    for h2 in [false, true] {
        let (status, body) =
            ask(&pki, proxy.https, (name, name, h2), Some(copy(&device))).await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains(&told(&device)), "the certificate verified, not the claim: {body}");
        assert!(body.contains("client-cert-chain=\n"), "it sent no chain beyond it: {body}");
    }
    let none = ask(&pki, proxy.https, (name, name, false), None).await;
    assert!(none.is_err(), "no certificate, no session: {none:?}");
    let stranger = ask(&pki, proxy.https, (name, name, false), Some(strangers.client("x"))).await;
    assert!(stranger.is_err(), "another authority's: {stranger:?}");
}

#[tokio::test]
async fn an_optional_endpoint_serves_a_client_without_one_and_tells_its_workload_none_was_given() {
    let (pki, clients) = (Pki::new(), Pki::new());
    let portal = workload("portal", None).await;
    let name = "portal.example.test";
    let proxy = Proxy::start(&pb::IngressConfig {
        routes: vec![asking(name, portal, C::CLIENT_CERTIFICATES_OPTIONAL, &clients.authority())],
        certificates: vec![pki.certificate("crt_portal.example.test", &[name])],
        ..Default::default()
    });
    let (status, body) = ask(&pki, proxy.https, (name, name, true), None).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("client-cert=\n"), "none given, and the claim removed: {body}");
    let device = clients.client("device-8");
    let (_, body) = ask(&pki, proxy.https, (name, name, true), Some(copy(&device))).await.unwrap();
    assert!(body.contains(&told(&device)), "{body}");
}

#[tokio::test]
async fn a_connection_made_for_a_name_asking_nothing_reaches_no_name_that_requires_a_certificate() {
    let (pki, clients) = (Pki::new(), Pki::new());
    let (open, locked) = ("www.example.test", "admin.example.test");
    let site = workload("site", None).await;
    let admin = workload("admin", None).await;
    let mut admin_route =
        asking(locked, admin, C::CLIENT_CERTIFICATES_REQUIRED, &clients.authority());
    admin_route.certificate = "crt_both".into();
    let proxy = Proxy::start(&pb::IngressConfig {
        routes: vec![route(&[open], site, pb::RouteMode::ROUTE_MODE_HTTP, "crt_both"), admin_route],
        certificates: vec![pki.certificate("crt_both", &[open, locked])],
        ..Default::default()
    });
    for h2 in [false, true] {
        let (status, body) = ask(&pki, proxy.https, (open, open, h2), None).await.unwrap();
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("workload=site") && body.contains("client-cert=\n"), "{body}");
        let (status, body) = ask(&pki, proxy.https, (open, locked, h2), None).await.unwrap();
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "the coalesced request: {body}");
    }
}
