// SPDX-License-Identifier: AGPL-3.0-or-later

//! The headers a request is forwarded with (§3.2 and §3.4 of docs/07-networking.md). Whatever a
//! client said about where it came from is removed, `Forwarded` (RFC 7239) and every
//! `X-Forwarded-*`, and the proxy sets its own, so the workload learns its client's address from
//! headers only the proxy writes. So it is with the certificate a client presented (RFC 9440):
//! `Client-Cert` and `Client-Cert-Chain` are removed from every request, and set only from the
//! certificate the connection's handshake verified. Hop-by-hop headers (RFC 9110 §7.6.1) describe
//! one connection and are not forwarded, save the upgrade a WebSocket asks for.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use rustls::pki_types::CertificateDer;
use std::net::{IpAddr, SocketAddr};

/// The end-entity certificate a client presented (RFC 9440 §2.2).
const CLIENT_CERT: HeaderName = HeaderName::from_static("client-cert");
/// The rest of the chain it presented, without the end-entity certificate (RFC 9440 §2.3).
const CLIENT_CERT_CHAIN: HeaderName = HeaderName::from_static("client-cert-chain");

/// Hop-by-hop headers, which describe the connection they arrived on.
const HOP_BY_HOP: [HeaderName; 7] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
];

/// Removes the hop-by-hop headers of `h`, every header its `Connection` names among them, and
/// `Upgrade` unless `upgrading`.
pub fn strip_hop_by_hop(h: &mut HeaderMap, upgrading: bool) {
    let named: Vec<HeaderName> = h
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|n| HeaderName::from_bytes(n.trim().as_bytes()).ok())
        .collect();
    for name in named.iter().chain(HOP_BY_HOP.iter()) {
        if !(upgrading && name == header::UPGRADE) {
            h.remove(name);
        }
    }
    if !upgrading {
        h.remove(header::UPGRADE);
    }
}

/// Whether `name` says where a request came from.
fn forwarding(name: &HeaderName) -> bool {
    name == header::FORWARDED || name.as_str().starts_with("x-forwarded-")
}

/// Replaces every forwarding header of `h` with the proxy's own: the client was `client`, it
/// asked for `host`, over `scheme`.
pub fn set_forwarded(h: &mut HeaderMap, client: SocketAddr, host: &str, scheme: &str) {
    let stale: Vec<HeaderName> = h.keys().filter(|n| forwarding(n)).cloned().collect();
    for name in stale {
        h.remove(name);
    }
    let node = match client.ip() {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("\"[{v6}]\""),
    };
    let set = |h: &mut HeaderMap, name: HeaderName, value: String| {
        if let Ok(v) = HeaderValue::from_str(&value) {
            h.insert(name, v);
        }
    };
    set(h, header::FORWARDED, format!("for={node};host={host};proto={scheme}"));
    set(h, HeaderName::from_static("x-forwarded-for"), client.ip().to_string());
    set(h, HeaderName::from_static("x-forwarded-host"), host.to_owned());
    set(h, HeaderName::from_static("x-forwarded-proto"), scheme.to_owned());
}

/// Replaces whatever `h` says of a client certificate with `presented`, the chain the
/// connection's handshake verified, end-entity first: each certificate a structured field Byte
/// Sequence of its DER, the chain's a List of them; neither where none was presented.
pub fn set_client_cert(h: &mut HeaderMap, presented: Option<&[CertificateDer<'static>]>) {
    h.remove(CLIENT_CERT);
    h.remove(CLIENT_CERT_CHAIN);
    let Some((end_entity, chain)) = presented.and_then(<[_]>::split_first) else { return };
    let item = |c: &CertificateDer<'_>| format!(":{}:", STANDARD.encode(c.as_ref()));
    if let Ok(v) = HeaderValue::from_str(&item(end_entity)) {
        h.insert(CLIENT_CERT, v);
    }
    let listed: Vec<String> = chain.iter().map(item).collect();
    if !listed.is_empty()
        && let Ok(v) = HeaderValue::from_str(&listed.join(", "))
    {
        h.insert(CLIENT_CERT_CHAIN, v);
    }
}

/// Whether `h` asks to upgrade its HTTP/1.1 connection to a WebSocket.
#[must_use]
pub fn wants_websocket(h: &HeaderMap) -> bool {
    let has = |name: header::HeaderName, token: &str| {
        h.get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(token)))
    };
    has(header::CONNECTION, "upgrade") && has(header::UPGRADE, "websocket")
}

/// Whether `req`, on HTTP/2, opens a WebSocket by extended CONNECT (RFC 8441 §4): a `CONNECT`
/// whose `:protocol` is `websocket`.
pub fn wants_websocket_h2<B>(req: &http::Request<B>) -> bool {
    req.method() == http::Method::CONNECT
        && req
            .extensions()
            .get::<hyper::ext::Protocol>()
            .is_some_and(|p| p.as_str().eq_ignore_ascii_case("websocket"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clients_say_about_where_it_came_from_is_replaced_by_the_proxys() {
        let mut h = HeaderMap::new();
        h.insert(header::FORWARDED, HeaderValue::from_static("for=10.0.0.1"));
        h.insert("x-forwarded-for", HeaderValue::from_static("10.0.0.1"));
        h.insert("x-forwarded-port", HeaderValue::from_static("1"));
        h.insert("x-real-ip", HeaderValue::from_static("kept: not a forwarding header"));
        let client: SocketAddr = "[2001:db8::7]:5555".parse().unwrap();
        set_forwarded(&mut h, client, "shop.example.com", "https");
        let forwarded = h.get(header::FORWARDED).unwrap();
        assert_eq!(forwarded, "for=\"[2001:db8::7]\";host=shop.example.com;proto=https");
        assert_eq!(h.get("x-forwarded-for").unwrap(), "2001:db8::7");
        assert!(h.get("x-forwarded-port").is_none());
        assert!(h.get("x-real-ip").is_some());
    }

    #[test]
    fn a_client_certificate_is_said_only_as_the_handshake_verified_it() {
        let mut h = HeaderMap::new();
        h.insert(CLIENT_CERT, HeaderValue::from_static(":Zm9yZ2Vk:"));
        h.insert(CLIENT_CERT_CHAIN, HeaderValue::from_static(":Zm9yZ2Vk:"));
        set_client_cert(&mut h, None);
        assert!(h.is_empty(), "a client's own say is removed: {h:?}");
        let chain = [CertificateDer::from(vec![1, 2, 3]), CertificateDer::from(vec![4])];
        set_client_cert(&mut h, Some(&chain[..1]));
        assert_eq!(h.get(CLIENT_CERT).unwrap(), ":AQID:");
        assert!(h.get(CLIENT_CERT_CHAIN).is_none(), "no chain beyond the end entity");
        set_client_cert(&mut h, Some(&chain));
        assert_eq!(h.get(CLIENT_CERT_CHAIN).unwrap(), ":BA==:");
        let chain = [chain[0].clone(), chain[1].clone(), CertificateDer::from(vec![5])];
        set_client_cert(&mut h, Some(&chain));
        assert_eq!(h.get(CLIENT_CERT_CHAIN).unwrap(), ":BA==:, :BQ==:");
    }

    #[test]
    fn hop_by_hop_headers_stay_on_their_hop_save_a_websockets_upgrade() {
        let mut h = HeaderMap::new();
        h.insert(header::CONNECTION, HeaderValue::from_static("Upgrade, x-private"));
        h.insert("x-private", HeaderValue::from_static("1"));
        h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        h.insert(header::TE, HeaderValue::from_static("trailers"));
        assert!(wants_websocket(&h));
        let mut upgrading = h.clone();
        strip_hop_by_hop(&mut upgrading, true);
        assert!(upgrading.get(header::UPGRADE).is_some() && upgrading.get("x-private").is_none());
        strip_hop_by_hop(&mut h, false);
        assert!(h.is_empty(), "{h:?}");
    }
}
