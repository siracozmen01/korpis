// SPDX-License-Identifier: AGPL-3.0-or-later

//! A `passthrough` route: the proxy reads only the name the client asks for and splices the
//! connection to the workload, which terminates TLS itself with its own certificate; and, where
//! its endpoint declared it, first tells the workload who connected in a PROXY header, version 2,
//! read here by an implementation of the protocol that is not Korpis's.

use crate::support::{Pki, Proxy, get, https, route, serve_one, server_tls, tls_over, workload};
use http::StatusCode;
use http_body_util::BodyExt as _;
use hyper_util::rt::TokioIo;
use korpis_proto::korpis::ingress::v1 as pb;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::AsyncReadExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

#[tokio::test]
async fn a_passthrough_workload_terminates_its_own_tls() {
    let pki = Pki::new();
    let own = server_tls(&pki.leaf(&["tls.example.test"]));
    let backend = workload("own-tls", Some(own)).await;
    let mode = pb::RouteMode::ROUTE_MODE_PASSTHROUGH;
    let config = pb::IngressConfig {
        routes: vec![route(&["tls.example.test"], backend, mode, "")],
        ..Default::default()
    };
    let proxy = Proxy::start(&config);
    let (status, body) =
        https(&pki, proxy.https, "tls.example.test", false, get("/", "tls.example.test")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("workload=own-tls"), "{body}");
    assert!(body.contains("forwarded=\n"), "unread: the proxy added nothing: {body}");
}

/// A workload that expects a PROXY header before its own TLS: its address, and the addresses each
/// header it is sent names.
async fn told(
    tls: Arc<rustls::ServerConfig>,
) -> (SocketAddr, mpsc::UnboundedReceiver<ppp::v2::Addresses>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut head = vec![0u8; 16];
            stream.read_exact(&mut head).await.unwrap();
            let mut rest = vec![0u8; usize::from(u16::from_be_bytes([head[14], head[15]]))];
            stream.read_exact(&mut rest).await.unwrap();
            head.extend_from_slice(&rest);
            let header = ppp::v2::Header::try_from(&head[..]).unwrap();
            assert_eq!(header.command, ppp::v2::Command::Proxy, "relayed for a client");
            assert_eq!(header.protocol, ppp::v2::Protocol::Stream);
            tx.send(header.addresses).unwrap();
            tokio::spawn(serve_one(stream, Some(Arc::clone(&tls)), "told"));
        }
    });
    (at, rx)
}

#[tokio::test]
async fn a_workload_that_declared_it_is_told_its_client_in_a_proxy_header_before_its_tls() {
    let pki = Pki::new();
    let (backend, mut headers) = told(server_tls(&pki.leaf(&["tls.example.test"]))).await;
    let mut tunnel =
        route(&["tls.example.test"], backend, pb::RouteMode::ROUTE_MODE_PASSTHROUGH, "");
    tunnel.proxy_protocol = true;
    let proxy = Proxy::start(&pb::IngressConfig { routes: vec![tunnel], ..Default::default() });

    let tcp = TcpStream::connect(proxy.https).await.unwrap();
    let client = tcp.local_addr().unwrap();
    let tls = tls_over(&pki, tcp, "tls.example.test", &[b"http/1.1"]).await.unwrap();
    let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await.unwrap();
    tokio::spawn(conn);
    let response = send.send_request(get("/", "tls.example.test")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "the workload's own TLS, after the header");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("workload=told"));
    let named = headers.recv().await.unwrap();
    assert_eq!(named, (client, proxy.https).into(), "the client, and the address it reached");
}
