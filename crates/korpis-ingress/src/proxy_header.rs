// SPDX-License-Identifier: AGPL-3.0-or-later

//! The PROXY protocol header, version 2 (§2.2 of `proxy-protocol.txt`, the specification
//! `HAProxy` publishes), which the proxy writes to a `passthrough` workload that declared it,
//! before the first byte it splices (§3.2 of docs/07-networking.md): the client's address and
//! port, and the address and port the client reached, over TCP. It carries no TLVs: the proxy has
//! read nothing of the connection but its SNI, which the workload reads itself.

use std::net::{IpAddr, SocketAddr};

/// The twelve bytes every version 2 header starts with.
const SIGNATURE: [u8; 12] = *b"\r\n\r\n\0\r\nQUIT\n";
/// Version 2, the command PROXY: the connection was relayed for a client.
const VERSION_PROXY: u8 = 0x21;
/// TCP over IPv4, and TCP over IPv6.
const TCP4: u8 = 0x11;
const TCP6: u8 = 0x21;

/// The header naming `client` as the connection's source and `reached` as its destination. Both
/// are IPv4 when both are, or are IPv4 mapped into IPv6 on a dual-stack socket; otherwise both
/// are written as IPv6, an IPv4 one mapped, since one header has one family.
#[must_use]
pub fn v2(client: SocketAddr, reached: SocketAddr) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + 36);
    out.extend_from_slice(&SIGNATURE);
    out.push(VERSION_PROXY);
    match (client.ip().to_canonical(), reached.ip().to_canonical()) {
        (IpAddr::V4(from), IpAddr::V4(to)) => {
            out.push(TCP4);
            out.extend_from_slice(&12u16.to_be_bytes());
            out.extend_from_slice(&from.octets());
            out.extend_from_slice(&to.octets());
        }
        (from, to) => {
            out.push(TCP6);
            out.extend_from_slice(&36u16.to_be_bytes());
            out.extend_from_slice(&six(from).octets());
            out.extend_from_slice(&six(to).octets());
        }
    }
    out.extend_from_slice(&client.port().to_be_bytes());
    out.extend_from_slice(&reached.port().to_be_bytes());
    out
}

fn six(ip: IpAddr) -> std::net::Ipv6Addr {
    match ip {
        IpAddr::V4(v4) => v4.to_ipv6_mapped(),
        IpAddr::V6(v6) => v6,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn an_ipv4_connection_is_named_in_the_bytes_the_specification_gives() {
        let header = v2("192.0.2.7:51000".parse().unwrap(), "198.51.100.1:443".parse().unwrap());
        let mut want = b"\r\n\r\n\0\r\nQUIT\n\x21\x11\x00\x0c".to_vec();
        want.extend_from_slice(&[192, 0, 2, 7, 198, 51, 100, 1, 0xc7, 0x38, 0x01, 0xbb]);
        assert_eq!(header, want);
    }

    #[test]
    fn a_dual_stack_socket_s_mapped_addresses_are_written_as_ipv4_and_a_mixed_pair_as_ipv6() {
        let mapped =
            v2("[::ffff:192.0.2.7]:1".parse().unwrap(), "[::ffff:10.0.0.1]:2".parse().unwrap());
        assert_eq!((mapped[13], mapped.len()), (TCP4, 16 + 12));
        let mixed = v2("[2001:db8::7]:1".parse().unwrap(), "10.0.0.1:443".parse().unwrap());
        assert_eq!((mixed[13], mixed.len()), (TCP6, 16 + 36));
        assert_eq!(
            mixed[32..48],
            "::ffff:10.0.0.1".parse::<std::net::Ipv6Addr>().unwrap().octets()
        );
    }
}
