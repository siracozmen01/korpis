// SPDX-License-Identifier: AGPL-3.0-or-later

//! HTTP/3 (§3.2 of docs/07-networking.md): a terminated route is served over QUIC on UDP 443,
//! with the certificate its name chooses, each request routed among the route's names and
//! forwarded over HTTP/1.1 as one over TCP is; a response over TCP advertises it; and a route
//! asking for client certificates is neither served over QUIC nor advertised there, its clients
//! keeping to TCP, where it asks for them.

use crate::support::{Pki, Proxy, connect, get, quic_client, route, workload};
use bytes::Buf as _;
use http::{StatusCode, header};
use hyper_util::rt::TokioIo;
use korpis_proto::korpis::ingress::v1 as pb;
use std::net::SocketAddr;

use pb::RouteMode as M;

/// `GET /h3` naming `authority` over HTTP/3 to the proxy at `at`, asking for `name`; the
/// response's status and body, or why there was none.
async fn h3_get(
    pki: &Pki,
    at: SocketAddr,
    (name, authority): (&str, &str),
) -> Result<(StatusCode, String), String> {
    let why = |e: &dyn std::fmt::Display| e.to_string();
    let mut endpoint =
        quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).map_err(|e| why(&e))?;
    endpoint.set_default_client_config(quic_client(pki));
    let connection = endpoint.connect(at, name).map_err(|e| why(&e))?.await.map_err(|e| why(&e))?;
    let (mut driver, mut send) =
        h3::client::new(h3_quinn::Connection::new(connection)).await.map_err(|e| why(&e))?;
    tokio::spawn(async move { driver.wait_idle().await });
    let req = http::Request::get(format!("https://{authority}/h3")).body(()).unwrap();
    let mut stream = send.send_request(req).await.map_err(|e| why(&e))?;
    stream.finish().await.map_err(|e| why(&e))?;
    let response = stream.recv_response().await.map_err(|e| why(&e))?;
    let mut body = Vec::new();
    while let Some(mut chunk) = stream.recv_data().await.map_err(|e| why(&e))? {
        body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
    }
    Ok((response.status(), String::from_utf8_lossy(&body).into_owned()))
}

/// What a response over TCP, HTTP/1.1, for `name` says of HTTP/3.
async fn advertised(pki: &Pki, at: SocketAddr, name: &str) -> Option<String> {
    let io = TokioIo::new(connect(pki, at, name, &[b"http/1.1"]).await.ok()?);
    let (mut send, conn) = hyper::client::conn::http1::handshake(io).await.ok()?;
    tokio::spawn(conn);
    let response = send.send_request(get("/", name)).await.ok()?;
    let alt = response.headers().get(header::ALT_SVC)?;
    alt.to_str().ok().map(str::to_owned)
}

#[tokio::test]
async fn a_terminated_route_is_served_over_http3_and_tcp_says_so() {
    let pki = Pki::new();
    let shop = workload("shop", None).await;
    let mut asking = route(&["vault.example.test"], shop, M::ROUTE_MODE_HTTP, "crt_vault");
    asking.client_certificates = pb::ClientCertificates::CLIENT_CERTIFICATES_REQUIRED.into();
    asking.client_ca = pki.authority();
    let config = pb::IngressConfig {
        routes: vec![route(&["shop.example.test"], shop, M::ROUTE_MODE_HTTP, "crt_shop"), asking],
        certificates: vec![
            pki.certificate("crt_shop", &["shop.example.test"]),
            pki.certificate("crt_vault", &["vault.example.test"]),
        ],
        ..Default::default()
    };
    let proxy = Proxy::start(&config);

    let (status, body) = h3_get(&pki, proxy.quic, ("shop.example.test", "shop.example.test"))
        .await
        .expect("served over HTTP/3");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("workload=shop") && body.contains("path=/h3"), "{body}");
    assert!(body.contains("version=HTTP/1.1"), "forwarded as any other: {body}");
    let other = h3_get(&pki, proxy.quic, ("shop.example.test", "vault.example.test")).await;
    assert_eq!(other.map(|(s, _)| s), Ok(StatusCode::MISDIRECTED_REQUEST));

    let alt = advertised(&pki, proxy.https, "shop.example.test").await;
    assert_eq!(alt.as_deref(), Some("h3=\":443\"; ma=86400"));
    let refused = h3_get(&pki, proxy.quic, ("vault.example.test", "vault.example.test")).await;
    assert!(refused.is_err(), "a route asking for client certificates keeps to TCP");
}
