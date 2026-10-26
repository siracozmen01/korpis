// SPDX-License-Identifier: AGPL-3.0-or-later

//! Announcing an address a node has just taken on a link (§3.1 of docs/07-networking.md), so the
//! link's neighbours and its router send to this node at once rather than once their entries for
//! the address's last holder go stale: for IPv4 a gratuitous ARP, a request and a reply each
//! naming the address at the link's own hardware address, broadcast; for IPv6 an unsolicited
//! neighbour advertisement from the address to every node of the link, its override flag set so
//! a neighbour replaces what it had (RFC 4861 §7.2.6). Each is sent whole on a packet socket,
//! whose link-layer address has no safe wrapper; this is one of the crate's unsafe modules.

#![expect(unsafe_code, reason = "a packet socket and its link-layer address have no safe wrapper")]

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

/// The `EtherType` of ARP.
const ETH_P_ARP: u16 = 0x0806;
/// The `EtherType` of IPv6.
const ETH_P_IPV6: u16 = 0x86dd;
const BROADCAST: [u8; 6] = [0xff; 6];
/// The hardware address of `ff02::1`, every node of the link.
const ALL_NODES: [u8; 6] = [0x33, 0x33, 0, 0, 0, 1];
/// The next-header number of ICMP for IPv6, and a neighbour advertisement's type.
const ICMPV6: u8 = 58;
const ADVERTISEMENT: u8 = 136;
/// A neighbour advertisement's override flag.
const OVERRIDE: u32 = 0x2000_0000;
const REQUEST: u16 = 1;
const REPLY: u16 = 2;

/// An ARP packet over Ethernet, of operation `op`, saying that `address` is at `mac`: the
/// target's hardware address is unknown in the request and the broadcast in the reply.
fn arp(op: u16, mac: [u8; 6], address: Ipv4Addr) -> Vec<u8> {
    let target = if op == REPLY { BROADCAST } else { [0; 6] };
    [
        &1_u16.to_be_bytes()[..],
        &0x0800_u16.to_be_bytes(),
        &[6, 4],
        &op.to_be_bytes(),
        &mac,
        &address.octets(),
        &target,
        &address.octets(),
    ]
    .concat()
}

/// The one's complement sum of `bytes` as 16-bit words, folded, as the Internet checksum adds.
fn summed(bytes: &[u8]) -> u32 {
    let mut sum = bytes.chunks(2).fold(0_u32, |s, w| {
        let word =
            u16::from_be_bytes([w.first().copied().unwrap_or(0), w.get(1).copied().unwrap_or(0)]);
        s + u32::from(word)
    });
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    sum
}

/// An unsolicited neighbour advertisement over Ethernet, an IPv6 packet from `address` to
/// `ff02::1` at hop limit 255, saying that `address` is at `mac`.
fn advertisement(mac: [u8; 6], address: Ipv6Addr) -> Vec<u8> {
    let all_nodes = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 1);
    // The flags, the target, and the target's link-layer address as its one option.
    let body = [&OVERRIDE.to_be_bytes()[..], &address.octets(), &[2, 1], &mac].concat();
    let length = u16::try_from(4 + body.len()).unwrap_or(u16::MAX);
    let pseudo = [
        &address.octets()[..],
        &all_nodes.octets(),
        &u32::from(length).to_be_bytes(),
        &[0, 0, 0, ICMPV6],
        &[ADVERTISEMENT, 0, 0, 0],
        &body,
    ]
    .concat();
    let checksum = !u16::try_from(summed(&pseudo)).unwrap_or(u16::MAX);
    [
        &[0x60, 0, 0, 0][..],
        &length.to_be_bytes(),
        &[ICMPV6, 255],
        &address.octets(),
        &all_nodes.octets(),
        &[ADVERTISEMENT, 0],
        &checksum.to_be_bytes(),
        &body,
    ]
    .concat()
}

/// A datagram packet socket for frames of `EtherType` `kind`.
fn socket(kind: u16) -> io::Result<OwnedFd> {
    let protocol = libc::c_int::from(kind.to_be());
    // SAFETY: socket(2) takes plain values and returns a new descriptor or -1.
    let fd =
        unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, protocol) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by socket(2), and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Sends `packet`, of `EtherType` `kind`, out of link `index` to hardware address `to`.
fn send(fd: &OwnedFd, (index, kind, to): (u32, u16, [u8; 6]), packet: &[u8]) -> io::Result<()> {
    let mut sll_addr = [0_u8; 8];
    for (to, from) in sll_addr.iter_mut().zip(to) {
        *to = from;
    }
    let to = libc::sockaddr_ll {
        sll_family: u16::try_from(libc::AF_PACKET).map_err(io::Error::other)?,
        sll_protocol: kind.to_be(),
        sll_ifindex: i32::try_from(index).map_err(io::Error::other)?,
        sll_hatype: 0,
        sll_pkttype: 0,
        sll_halen: 6,
        sll_addr,
    };
    let len = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_ll>())
        .map_err(io::Error::other)?;
    // SAFETY: `fd` is open for the whole call, because it is borrowed for it. The kernel reads
    // `packet.len()` bytes from `packet` and `len` bytes from `to`, a `sockaddr_ll` of exactly
    // that size, both borrowed for the call, and writes to neither.
    let sent = unsafe {
        libc::sendto(
            fd.as_raw_fd(),
            packet.as_ptr().cast(),
            packet.len(),
            0,
            std::ptr::from_ref(&to).cast(),
            len,
        )
    };
    if sent < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// Announces `address` on link `index`, whose hardware address is `mac`.
///
/// # Errors
/// The link is not Ethernet, or the kernel refused the socket or a packet.
pub(crate) fn announce(index: u32, mac: &[u8], address: IpAddr) -> io::Result<()> {
    let mac: [u8; 6] =
        mac.try_into().map_err(|_| io::Error::other("the link has no Ethernet address"))?;
    match address {
        IpAddr::V4(address) => {
            let fd = socket(ETH_P_ARP)?;
            for op in [REQUEST, REPLY] {
                send(&fd, (index, ETH_P_ARP, BROADCAST), &arp(op, mac, address))?;
            }
        }
        IpAddr::V6(address) => {
            let fd = socket(ETH_P_IPV6)?;
            send(&fd, (index, ETH_P_IPV6, ALL_NODES), &advertisement(mac, address))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn a_gratuitous_arp_names_the_address_as_both_sender_and_target() {
        let mac = [2, 0, 0, 0, 0, 1];
        let p = arp(REQUEST, mac, Ipv4Addr::new(198, 18, 1, 10));
        assert_eq!(p.len(), 28);
        assert_eq!(p.get(6..8).unwrap(), &[0, 1], "a request");
        assert_eq!(p.get(8..14).unwrap(), &mac);
        assert_eq!(p.get(14..18), p.get(24..28), "sender and target are the address");
        assert_eq!(arp(REPLY, mac, Ipv4Addr::LOCALHOST).get(18..24).unwrap(), &BROADCAST);
    }

    #[test]
    fn a_neighbour_advertisement_overrides_from_the_address_to_every_node_and_sums_to_zero() {
        let mac = [2, 0, 0, 0, 0, 1];
        let address: Ipv6Addr = "2001:db8::2".parse().unwrap();
        let p = advertisement(mac, address);
        assert_eq!(p.len(), 40 + 32, "the header, and the advertisement with its one option");
        assert_eq!((p[6], p[7]), (ICMPV6, 255), "ICMPv6 at the hop limit neighbours require");
        assert_eq!(p.get(8..24).unwrap(), &address.octets(), "from the address");
        assert_eq!(p[40], ADVERTISEMENT);
        assert_eq!(p[44] & 0x20, 0x20, "the override flag");
        assert_eq!(p.get(48..64).unwrap(), &address.octets(), "the target");
        assert_eq!(p.get(64..72).unwrap(), &[&[2, 1][..], &mac].concat()[..]);
        // The checksum over the pseudo-header and the message, checksum included, is all ones.
        let pseudo = [p.get(8..40).unwrap(), &[0, 0, 0, 32, 0, 0, 0, ICMPV6], p.get(40..).unwrap()];
        assert_eq!(summed(&pseudo.concat()), 0xffff);
    }
}
