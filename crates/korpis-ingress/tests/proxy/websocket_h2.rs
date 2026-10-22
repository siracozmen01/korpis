// SPDX-License-Identifier: AGPL-3.0-or-later

//! A WebSocket opened on HTTP/2 by extended CONNECT (RFC 8441, §3.2 of docs/07-networking.md):
//! the proxy announces it, forwards the stream to the workload as the HTTP/1.1 upgrade the
//! workload understands, answers the client 200 once the workload switches protocols, and joins
//! the two byte for byte; a request on the same connection still reaches the workload as any does.

use crate::support::{Pki, Proxy, connect, get, route, workload};
use bytes::Bytes;
use http::{Method, Request, StatusCode};
use http_body_util::Empty;
use hyper_util::rt::{TokioExecutor, TokioIo};
use korpis_proto::korpis::ingress::v1 as pb;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[tokio::test]
async fn a_websocket_opens_on_an_http2_connection_and_is_joined_to_its_workload() {
    let pki = Pki::new();
    let echo = workload("echo", None).await;
    let config = pb::IngressConfig {
        routes: vec![route(&["ws.example.test"], echo, pb::RouteMode::ROUTE_MODE_HTTP, "crt")],
        certificates: vec![pki.certificate("crt", &["ws.example.test"])],
        ..Default::default()
    };
    let proxy = Proxy::start(&config);
    let tls = connect(&pki, proxy.https, "ws.example.test", &[b"h2"]).await.unwrap();
    let (mut send, conn) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tls))
            .await
            .unwrap();
    tokio::spawn(conn);
    // A request first, so the proxy's settings, which allow extended CONNECT, have arrived.
    let first = send.send_request(get("/", "ws.example.test")).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let mut open = Request::builder()
        .method(Method::CONNECT)
        .uri("https://ws.example.test/chat")
        .header("sec-websocket-version", "13")
        .body(Empty::<Bytes>::new())
        .unwrap();
    open.extensions_mut().insert(hyper::ext::Protocol::from_static("websocket"));
    let mut opened = send.send_request(open).await.unwrap();
    assert_eq!(opened.status(), StatusCode::OK, "HTTP/2 says 200 where HTTP/1.1 says 101");
    let mut stream = TokioIo::new(hyper::upgrade::on(&mut opened).await.unwrap());
    stream.write_all(b"ping").await.unwrap();
    let mut echoed = [0u8; 4];
    stream.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ping", "joined byte for byte");
}
