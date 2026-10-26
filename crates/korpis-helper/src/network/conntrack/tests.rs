// SPDX-License-Identifier: AGPL-3.0-or-later

#![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

use super::*;

/// An attribute of `kind` holding `payload`, padded.
fn attr(kind: u16, payload: &[u8]) -> Vec<u8> {
    let len = u16::try_from(4 + payload.len()).unwrap();
    let mut out = [len.to_ne_bytes(), kind.to_ne_bytes()].concat();
    out.extend_from_slice(payload);
    out.resize(out.len().next_multiple_of(4), 0);
    out
}

fn nested(kind: u16, parts: &[Vec<u8>]) -> Vec<u8> {
    attr(kind | 0x8000, &parts.concat())
}

fn octets(a: IpAddr) -> Vec<u8> {
    match a {
        IpAddr::V4(a) => a.octets().to_vec(),
        IpAddr::V6(a) => a.octets().to_vec(),
    }
}

fn encoded(t: Tuple) -> Vec<u8> {
    let (src, dst) = if t.source.0.is_ipv4() {
        (CTA_IP_V4_SRC, CTA_IP_V4_DST)
    } else {
        (CTA_IP_V6_SRC, CTA_IP_V6_DST)
    };
    let ip = nested(
        CTA_TUPLE_IP,
        &[attr(src, &octets(t.source.0)), attr(dst, &octets(t.destination.0))],
    );
    let proto = nested(
        CTA_TUPLE_PROTO,
        &[
            attr(CTA_PROTO_NUM, &[t.protocol]),
            attr(CTA_PROTO_SRC_PORT, &t.source.1.to_be_bytes()),
            attr(CTA_PROTO_DST_PORT, &t.destination.1.to_be_bytes()),
        ],
    );
    [ip, proto].concat()
}

#[test]
fn a_translated_connection_is_read_with_where_it_went_and_named_by_its_original_tuple() {
    let original = Tuple {
        source: ("198.18.1.2".parse().unwrap(), 40_000),
        destination: ("198.18.1.10".parse().unwrap(), 25_565),
        protocol: 17,
    };
    let reply = Tuple {
        source: ("10.48.4.2".parse().unwrap(), 25_565),
        destination: ("198.18.1.2".parse().unwrap(), 40_000),
        protocol: 17,
    };
    let orig = nested(CTA_TUPLE_ORIG, &[encoded(original)]);
    let body =
        [orig.clone(), nested(CTA_TUPLE_REPLY, &[encoded(reply)]), attr(3, &[0; 4])].concat();
    let c = connection(&body).unwrap();
    assert_eq!((c.original, c.reply), (original, reply));
    assert_eq!(c.raw, orig, "forgotten by the tuple exactly as the kernel gave it");
    assert!(connection(body.get(..8).unwrap()).is_none(), "a short message is no connection");
}

#[test]
fn an_ipv6_connection_is_read_as_an_ipv4_one_is() {
    let ip = |a: &str| a.parse::<IpAddr>().unwrap();
    let original = Tuple {
        source: (ip("2001:db8:9::2"), 40_000),
        destination: (ip("2001:db8::2"), 25_565),
        protocol: 17,
    };
    let reply = Tuple {
        source: (ip("fd00::2"), 25_565),
        destination: (ip("2001:db8:9::2"), 40_000),
        protocol: 17,
    };
    let body =
        [nested(CTA_TUPLE_ORIG, &[encoded(original)]), nested(CTA_TUPLE_REPLY, &[encoded(reply)])]
            .concat();
    let c = connection(&body).unwrap();
    assert_eq!((c.original, c.reply), (original, reply));
}
