// SPDX-License-Identifier: AGPL-3.0-or-later

//! The proxy, spoken to as a client speaks to it: a request without its credential is refused; a
//! tunnel or a request to an address the policy does not permit is refused before anything is
//! sent there; one it permits is carried; a plain HTTP request is forwarded alone, in origin form,
//! without the proxy's credential, and its connection closed once it is answered.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "a failing test should fail loudly")]

use base64::Engine as _;
use korpis_egress::{Proxy, Reach};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

/// A server on loopback that records the first bytes of each connection and answers `answer`;
/// its port, and what it was sent.
async fn server(answer: &'static [u8]) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).await.unwrap_or(0);
            record
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(buf.get(..n).unwrap_or_default()).into_owned());
            let _ = s.write_all(answer).await;
        }
    });
    (port, seen)
}

/// The proxy's address and its `Proxy-Authorization` header, from its URL.
fn parts(proxy: &Proxy) -> (String, String) {
    let url = reqwest::Url::parse(proxy.url()).unwrap();
    let at = format!("127.0.0.1:{}", url.port().unwrap());
    let pair = format!("{}:{}", url.username(), url.password().unwrap());
    (at, format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(pair)))
}

/// Sends `request` to the proxy and reads until it closes or `limit` bytes.
async fn ask(proxy: &Proxy, request: &str) -> String {
    let (at, _) = parts(proxy);
    let mut s = TcpStream::connect(at).await.unwrap();
    s.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

const OK: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello";

#[tokio::test]
async fn a_request_without_the_proxy_s_credential_is_refused() {
    let proxy = Proxy::start(Reach::default(), Vec::new()).unwrap();
    let (port, seen) = server(OK).await;
    let answer = ask(&proxy, &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n")).await;
    assert!(answer.starts_with("HTTP/1.1 407"), "{answer}");
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn loopback_is_reached_only_when_the_operator_allows_it() {
    let (port, seen) = server(OK).await;
    let refused = Proxy::start(Reach::default(), Vec::new()).unwrap();
    let (_, credential) = parts(&refused);
    for target in [format!("127.0.0.1:{port}"), format!("localhost:{port}")] {
        let req = format!("CONNECT {target} HTTP/1.1\r\nproxy-authorization: {credential}\r\n\r\n");
        let answer = ask(&refused, &req).await;
        assert!(answer.starts_with("HTTP/1.1 403"), "{target}: {answer}");
    }
    let plain = format!(
        "GET http://127.0.0.1:{port}/x HTTP/1.1\r\nhost: 127.0.0.1\r\nproxy-authorization: \
         {credential}\r\n\r\n"
    );
    assert!(ask(&refused, &plain).await.starts_with("HTTP/1.1 403"));
    assert!(seen.lock().unwrap().is_empty(), "nothing reached the server");

    let reach = Reach::with(vec!["127.0.0.1/32".parse().unwrap()]);
    let allowed = Proxy::start(reach, Vec::new()).unwrap();
    let (_, credential) = parts(&allowed);
    let req = format!(
        "CONNECT 127.0.0.1:{port} HTTP/1.1\r\nproxy-authorization: {credential}\r\n\r\nping"
    );
    let answer = ask(&allowed, &req).await;
    assert!(answer.starts_with("HTTP/1.1 200 Connection Established"), "{answer}");
    assert!(answer.ends_with("hello"), "the tunnel carries both ways: {answer}");
    assert_eq!(seen.lock().unwrap().as_slice(), ["ping"]);
}

#[tokio::test]
async fn a_named_registry_is_reached_over_plain_http_one_request_to_a_connection() {
    let (port, seen) = server(OK).await;
    let proxy = Proxy::start(Reach::default(), vec![format!("127.0.0.1:{port}")]).unwrap();
    let (_, credential) = parts(&proxy);
    let req = format!(
        "GET http://127.0.0.1:{port}/v2/?n=1 HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\
         proxy-authorization: {credential}\r\nconnection: keep-alive\r\n\r\n"
    );
    let answer = ask(&proxy, &req).await;
    assert!(answer.ends_with("hello"), "{answer}");
    let sent = seen.lock().unwrap().clone();
    let head = sent.first().unwrap();
    assert!(head.starts_with("GET /v2/?n=1 HTTP/1.1\r\n"), "origin form: {head}");
    assert!(!head.to_ascii_lowercase().contains("proxy-authorization"), "{head}");
    assert!(head.contains("connection: close\r\n") && !head.contains("keep-alive"), "{head}");
    let other = Proxy::start(Reach::default(), vec![format!("127.0.0.1:{}", port + 1)]).unwrap();
    let (_, credential) = parts(&other);
    let req = format!(
        "GET http://127.0.0.1:{port}/ HTTP/1.1\r\nproxy-authorization: {credential}\r\n\r\n"
    );
    assert!(ask(&other, &req).await.starts_with("HTTP/1.1 403"), "another port is not named");
}

#[tokio::test]
async fn a_client_that_takes_a_proxy_reaches_a_named_registry_through_it() {
    let (port, seen) = server(OK).await;
    let proxy = Proxy::start(Reach::default(), vec![format!("127.0.0.1:{port}")]).unwrap();
    // One TLS provider for the whole process, ring, as everywhere in Korpis.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .proxy(reqwest::Proxy::all(proxy.url()).unwrap())
        .build()
        .unwrap();
    let body = client.get(format!("http://127.0.0.1:{port}/v2/")).send().await.unwrap();
    assert_eq!(body.text().await.unwrap(), "hello");
    let refused = client.get("http://127.0.0.1:9/").send().await.unwrap();
    assert_eq!(refused.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(seen.lock().unwrap().len(), 1);
}
