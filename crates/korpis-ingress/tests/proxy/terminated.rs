// SPDX-License-Identifier: AGPL-3.0-or-later

//! Routes the proxy terminates: TLS chosen by SNI, requests routed among the route's names and
//! forwarded over HTTP/1.1 with the proxy's word on where they came from, WebSocket upgrades joined, and
//! every bound answered with the status that says which.

use crate::support::{Pki, Proxy, connect, get, https, route, server_tls, workload};
use http::{StatusCode, header};
use korpis_proto::korpis::ingress::v1 as pb;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use pb::RouteMode as M;

#[tokio::test]
async fn a_request_reaches_its_workload_with_the_proxys_word_on_where_it_came_from() {
    let pki = Pki::new();
    let shop = workload("shop", None).await;
    let names = ["shop.example.test", "www.example.test"];
    let config = pb::IngressConfig {
        routes: vec![route(&names, shop, M::ROUTE_MODE_HTTP, "crt_shop")],
        certificates: vec![pki.certificate("crt_shop", &names)],
        ..Default::default()
    };
    let proxy = Proxy::start(&config);
    for h2 in [false, true] {
        let mut req = get("https://www.example.test/cart?id=7", "www.example.test");
        req.headers_mut().insert("x-forwarded-for", "6.6.6.6".parse().unwrap());
        req.headers_mut().insert(header::FORWARDED, "for=6.6.6.6".parse().unwrap());
        let (status, body) = https(&pki, proxy.https, "shop.example.test", h2, req).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains("workload=shop"), "{body}");
        assert!(body.contains("path=/cart?id=7") && body.contains("version=HTTP/1.1"), "{body}");
        assert!(body.contains("host=www.example.test\n"), "the name asked for: {body}");
        let forwarded = "forwarded=for=127.0.0.1;host=www.example.test;proto=https\n";
        assert!(body.contains(forwarded), "only the proxy's: {body}");
        assert!(body.contains("x-forwarded-for=127.0.0.1\n"), "{body}");
    }
}

#[tokio::test]
async fn a_name_the_connection_was_not_made_for_is_misdirected_and_an_unrouted_one_has_no_handshake()
 {
    let pki = Pki::new();
    let (a, b) = (workload("a", None).await, workload("b", None).await);
    let config = pb::IngressConfig {
        routes: vec![
            route(&["a.example.test"], a, M::ROUTE_MODE_HTTP, "crt_a"),
            route(&["b.example.test"], b, M::ROUTE_MODE_HTTP, "crt_b"),
            route(&["c.example.test"], b, M::ROUTE_MODE_HTTP, ""),
        ],
        certificates: vec![
            pki.certificate("crt_a", &["a.example.test"]),
            pki.certificate("crt_b", &["b.example.test"]),
        ],
        ..Default::default()
    };
    let proxy = Proxy::start(&config);
    let req = get("/", "b.example.test");
    let (status, _) = https(&pki, proxy.https, "a.example.test", false, req).await;
    assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
    for name in ["nobody.example.test", "c.example.test"] {
        assert!(connect(&pki, proxy.https, name, &[]).await.is_err(), "{name}: no certificate");
    }
}

#[tokio::test]
async fn a_workload_serving_its_own_certificate_is_reached_within_tls() {
    let pki = Pki::new();
    let own = server_tls(&pki.leaf(&["whatever.internal"]));
    let secure = workload("secure", Some(own)).await;
    let config = pb::IngressConfig {
        routes: vec![route(&["s.example.test"], secure, M::ROUTE_MODE_HTTPS, "crt_s")],
        certificates: vec![pki.certificate("crt_s", &["s.example.test"])],
        ..Default::default()
    };
    let proxy = Proxy::start(&config);
    let (status, body) =
        https(&pki, proxy.https, "s.example.test", true, get("/", "s.example.test")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("workload=secure"), "{body}");
}

#[tokio::test]
async fn a_websocket_is_joined_to_its_workload_and_an_absent_workload_is_502() {
    let pki = Pki::new();
    let echo = workload("echo", None).await;
    let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let config = pb::IngressConfig {
        routes: vec![
            route(&["ws.example.test"], echo, M::ROUTE_MODE_HTTP, "crt_ws"),
            route(&["gone.example.test"], gone, M::ROUTE_MODE_HTTP, "crt_gone"),
        ],
        certificates: vec![
            pki.certificate("crt_ws", &["ws.example.test"]),
            pki.certificate("crt_gone", &["gone.example.test"]),
        ],
        ..Default::default()
    };
    let proxy = Proxy::start(&config);
    let mut tls = connect(&pki, proxy.https, "ws.example.test", &[b"http/1.1"]).await.unwrap();
    let upgrade = "GET /chat HTTP/1.1\r\nHost: ws.example.test\r\nConnection: Upgrade\r\n\
                   Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
                   Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
    tls.write_all(upgrade.as_bytes()).await.unwrap();
    let mut head = vec![0u8; 1024];
    let n = tls.read(&mut head).await.unwrap();
    let head = String::from_utf8_lossy(&head[..n]).to_lowercase();
    assert!(head.starts_with("http/1.1 101"), "{head}");
    tls.write_all(b"ping").await.unwrap();
    let mut echoed = [0u8; 4];
    tls.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"ping", "joined byte for byte");

    let req = get("/", "gone.example.test");
    let (status, _) = https(&pki, proxy.https, "gone.example.test", false, req).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn a_head_above_the_bound_is_refused_431() {
    let pki = Pki::new();
    let shop = workload("shop", None).await;
    let config = pb::IngressConfig {
        routes: vec![route(&["shop.example.test"], shop, M::ROUTE_MODE_HTTP, "crt")],
        certificates: vec![pki.certificate("crt", &["shop.example.test"])],
        limits: pb::Limits { max_headers: 8, ..Default::default() }.into(),
        ..Default::default()
    };
    let proxy = Proxy::start(&config);
    let mut req = get("/", "shop.example.test");
    for i in 0..20 {
        req.headers_mut().insert(
            http::HeaderName::try_from(format!("x-filler-{i}")).unwrap(),
            "x".parse().unwrap(),
        );
    }
    let (status, _) = https(&pki, proxy.https, "shop.example.test", false, req).await;
    assert_eq!(status, StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
}
