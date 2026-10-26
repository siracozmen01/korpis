// SPDX-License-Identifier: AGPL-3.0-or-later

//! Announcing an address a node has just taken on a link (§3.1 of docs/07-networking.md): a
//! gratuitous ARP, a request and a reply each naming the address at the link's own hardware
//! address, broadcast, so the link's neighbours and its router send to this node at once rather
//! than once their entries for the address's last holder go stale. It is sent on a packet socket,
//! whose link-layer address has no safe wrapper; this is one of the crate's unsafe modules.

#![expect(unsafe_code, reason = "a packet socket and its link-layer address have no safe wrapper")]

use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

/// The `EtherType` of ARP.
const ETH_P_ARP: u16 = 0x0806;
const BROADCAST: [u8; 6] = [0xff; 6];
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

/// A datagram packet socket for ARP.
fn socket() -> io::Result<OwnedFd> {
    let protocol = libc::c_int::from(ETH_P_ARP.to_be());
    // SAFETY: socket(2) takes plain values and returns a new descriptor or -1.
    let fd =
        unsafe { libc::socket(libc::AF_PACKET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, protocol) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just returned by socket(2), and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Sends `packet` out of link `index` to the link's broadcast address.
fn send(fd: &OwnedFd, index: u32, packet: &[u8]) -> io::Result<()> {
    let to = libc::sockaddr_ll {
        sll_family: u16::try_from(libc::AF_PACKET).map_err(io::Error::other)?,
        sll_protocol: ETH_P_ARP.to_be(),
        sll_ifindex: i32::try_from(index).map_err(io::Error::other)?,
        sll_hatype: 0,
        sll_pkttype: 0,
        sll_halen: 6,
        sll_addr: [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0, 0],
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
pub(crate) fn announce(index: u32, mac: &[u8], address: Ipv4Addr) -> io::Result<()> {
    let mac: [u8; 6] =
        mac.try_into().map_err(|_| io::Error::other("the link has no Ethernet address"))?;
    let fd = socket()?;
    for op in [REQUEST, REPLY] {
        send(&fd, index, &arp(op, mac, address))?;
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
}
