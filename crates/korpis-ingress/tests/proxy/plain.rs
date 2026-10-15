// SPDX-License-Identifier: AGPL-3.0-or-later

//! Port 80: ACME's HTTP challenge answered from the tokens the agent gave, everything else for a
//! routed name redirected to HTTPS, and connections beyond the bound answered 503.

use crate::support::{Proxy, raw, route};
use korpis_proto::korpis::ingress::v1 as pb;
use tokio::io::AsyncWriteExt as _;

fn config(max_connections: u32) -> pb::IngressConfig {
    let backend = "127.0.0.1:9".parse().unwrap();
    pb::IngressConfig {
        routes: vec![route(&["shop.example.test"], backend, pb::RouteMode::ROUTE_MODE_HTTP, "")],
        challenges: vec![pb::Challenge {
            token: "tok-1".into(),
            key_authorization: "tok-1.thumbprint".into(),
            ..Default::default()
        }],
        limits: pb::Limits { max_connections, ..Default::default() }.into(),
        ..Default::default()
    }
}

fn request(path: &str, host: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
}

#[tokio::test]
async fn port_80_answers_challenges_and_redirects_what_it_routes() {
    let proxy = Proxy::start(&config(0));
    let challenge =
        raw(proxy.http, &request("/.well-known/acme-challenge/tok-1", "any.example")).await;
    assert!(challenge.starts_with("HTTP/1.1 200"), "{challenge}");
    assert!(challenge.ends_with("tok-1.thumbprint"), "{challenge}");
    let unknown =
        raw(proxy.http, &request("/.well-known/acme-challenge/nope", "shop.example.test")).await;
    assert!(unknown.starts_with("HTTP/1.1 404"), "{unknown}");
    let moved = raw(proxy.http, &request("/cart?id=7", "Shop.Example.test:80")).await;
    assert!(moved.starts_with("HTTP/1.1 308"), "{moved}");
    assert!(moved.contains("location: https://shop.example.test/cart?id=7\r\n"), "{moved}");
    let elsewhere = raw(proxy.http, &request("/", "other.example.test")).await;
    assert!(elsewhere.starts_with("HTTP/1.1 404"), "{elsewhere}");
}

#[tokio::test]
async fn connections_beyond_the_bound_are_answered_503_and_the_bound_follows_the_table() {
    let proxy = Proxy::start(&config(1));
    let mut held = tokio::net::TcpStream::connect(proxy.http).await.unwrap();
    held.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let busy = raw(proxy.http, &request("/", "shop.example.test")).await;
    assert!(busy.starts_with("HTTP/1.1 503"), "{busy}");
    proxy.set(&config(2));
    let served = raw(proxy.http, &request("/", "shop.example.test")).await;
    assert!(served.starts_with("HTTP/1.1 308"), "{served}");
    drop(held);
}

#[tokio::test]
async fn a_connection_that_says_nothing_is_closed_at_the_bound() {
    use tokio::io::AsyncReadExt as _;
    let mut quick = config(0);
    quick.limits =
        pb::Limits { idle_timeout_ms: 200, header_timeout_ms: 5000, ..Default::default() }.into();
    let proxy = Proxy::start(&quick);
    let mut silent = tokio::net::TcpStream::connect(proxy.http).await.unwrap();
    let started = std::time::Instant::now();
    let mut buf = [0u8; 16];
    let closed = tokio::time::timeout(std::time::Duration::from_secs(3), silent.read(&mut buf));
    assert!(matches!(closed.await, Ok(Ok(0) | Err(_))), "closed, not answered");
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
}
