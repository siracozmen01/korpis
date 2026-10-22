// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use std::sync::Arc;

/// The first flight of a real rustls client connecting to `name`.
fn hello(name: &str) -> Vec<u8> {
    offering(name, &[])
}

/// The first flight of a real rustls client connecting to `name`, offering `protocols`.
fn offering(name: &str, protocols: &[&[u8]]) -> Vec<u8> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    config.alpn_protocols = protocols.iter().map(|p| p.to_vec()).collect();
    let server = rustls::pki_types::ServerName::try_from(name.to_owned()).unwrap();
    let mut client = rustls::ClientConnection::new(Arc::new(config), server).unwrap();
    let mut out = Vec::new();
    client.write_tls(&mut out).unwrap();
    out
}

/// `wire` with its one record's payload cut into records of at most `n` bytes.
fn rechunked(wire: &[u8], n: usize) -> Vec<u8> {
    let (header, payload) = wire.split_at(5);
    let mut out = Vec::new();
    for part in payload.chunks(n) {
        out.extend_from_slice(&header[..3]);
        out.extend_from_slice(&u16::try_from(part.len()).unwrap().to_be_bytes());
        out.extend_from_slice(part);
    }
    out
}

#[tokio::test]
async fn the_name_a_client_asks_for_is_read_and_every_byte_kept() {
    let wire = hello("Shop.Example.com");
    let mut more = wire.clone();
    more.extend_from_slice(b"after");
    let mut r = more.as_slice();
    let h = read_hello(&mut r).await.unwrap();
    assert_eq!(h.name.as_deref(), Some("shop.example.com"));
    assert_eq!(h.read, wire, "exactly the hello, for a passthrough to replay");
    assert_eq!(r, b"after");

    let split = rechunked(&wire, 100);
    let h = read_hello(&mut split.as_slice()).await.unwrap();
    assert_eq!((h.name.as_deref(), h.read.len()), (Some("shop.example.com"), split.len()));

    let by_address = hello("192.0.2.1");
    assert_eq!(read_hello(&mut by_address.as_slice()).await.unwrap().name, None, "no SNI");
}

#[tokio::test]
async fn the_protocols_a_client_offers_say_whether_it_validates_a_name_by_tls_alpn() {
    let read = |wire: Vec<u8>| async move { read_hello(&mut wire.as_slice()).await.unwrap() };
    let validation = read(offering("shop.example.com", &[ACME_TLS])).await;
    assert_eq!(validation.protocols, [ACME_TLS.to_vec()]);
    assert!(validation.acme_only());
    let browser = read(offering("shop.example.com", &[b"h2", b"http/1.1"])).await;
    assert_eq!(browser.protocols, [b"h2".to_vec(), b"http/1.1".to_vec()]);
    assert!(!browser.acme_only());
    assert!(!read(offering("shop.example.com", &[ACME_TLS, b"h2"])).await.acme_only());
    assert!(!read(hello("shop.example.com")).await.acme_only(), "none offered");
}

#[tokio::test]
async fn what_is_not_a_hello_or_is_too_long_is_refused() {
    let plain = b"GET / HTTP/1.1\r\nHost: shop.example.com\r\n\r\n";
    assert!(read_hello(&mut plain.as_slice()).await.is_err());
    let mut wire = hello("shop.example.com");
    wire.truncate(wire.len() - 10);
    assert!(read_hello(&mut wire.as_slice()).await.is_err(), "cut short");
    let mut huge = vec![HANDSHAKE, 3, 1, 0x40, 0];
    huge.extend_from_slice(&[CLIENT_HELLO, 0x10, 0, 0]);
    huge.resize(5 + 0x4000, 0);
    let mut twice = huge.clone();
    twice.extend_from_slice(&huge);
    twice.extend_from_slice(&huge);
    let e = read_hello(&mut twice.as_slice()).await.unwrap_err();
    assert!(e.to_string().contains("limit"), "{e}");
}
