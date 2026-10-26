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

fn encoded(t: Tuple) -> Vec<u8> {
    let ip = nested(
        CTA_TUPLE_IP,
        &[
            attr(CTA_IP_V4_SRC, &t.source.0.octets()),
            attr(CTA_IP_V4_DST, &t.destination.0.octets()),
        ],
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
