// SPDX-License-Identifier: AGPL-3.0-or-later

//! A microVM's link (§4.1 of docs/07-networking.md): the guest's `eth0` is a tap, `tap0`, in its
//! workload's namespace, and the guest's own kernel holds the workload's addresses. The
//! namespace routes between the tap and its end of the link, as the host routes between the link
//! and its uplinks, and answers for each side on the other: on `eth0` for the workload's
//! addresses, by proxy ARP and a proxy NDP entry, and on the tap for the gateways, the same way,
//! so neither the host nor the guest learns that a hop stands between them. The namespace holds
//! no address of its own on either link, so it reaches the gateway through a permanent entry for
//! the host side's hardware address, never by asking.
//!
//! The tap is persistent, so it outlives the VMM that opens it, and owned by the workload's first
//! id, which the jailer runs the VMM as, so no other user of the host can attach to it. A tap has
//! no netlink request that makes one, only the `/dev/net/tun` ioctls, which have no safe wrapper:
//! this is one of the crate's unsafe modules.

#![expect(unsafe_code, reason = "the tun ioctls have no safe wrapper")]

use super::rtnl::Rtnl;
use super::{GATEWAY_V4, GATEWAY_V6, INNER};
use korpis_core::workload::network::TAP;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::AsRawFd as _;

/// The flags a tap is made with, which the VMM opens it with: Firecracker asks for frames with
/// no packet information and with the virtio network header.
const FLAGS: libc::c_int = libc::IFF_TAP | libc::IFF_NO_PI | libc::IFF_VNET_HDR;

/// Runs `request` on `fd` with the bytes of `args`, which are its structure.
fn ioctl_with(fd: &std::fs::File, request: libc::Ioctl, args: &mut [u8]) -> io::Result<()> {
    let ptr = args.as_mut_ptr();
    // SAFETY: `fd` is open for the whole call, because it is borrowed for it. `ptr` points to
    // `args`, exclusively borrowed for the call, which the one caller sizes and lays out as the
    // `struct ifreq` the request reads and writes: the kernel touches no byte outside it.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), request, ptr) };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// Runs `request` on `fd` with the plain value `arg`.
fn ioctl_value(fd: &std::fs::File, request: libc::Ioctl, arg: libc::c_ulong) -> io::Result<()> {
    // SAFETY: `fd` is open for the whole call, because it is borrowed for it, and the requests
    // passed here take their argument as a value, which the kernel never dereferences.
    let r = unsafe { libc::ioctl(fd.as_raw_fd(), request, arg) };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// Makes the persistent tap `tap0` in the calling thread's namespace, owned by `owner`.
fn make(owner: u32) -> io::Result<()> {
    let tun = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .map_err(|e| io::Error::new(e.kind(), format!("/dev/net/tun, the tun driver: {e}")))?;
    let mut ifreq = [0_u8; std::mem::size_of::<libc::ifreq>()];
    let short = libc::c_short::try_from(FLAGS).map_err(io::Error::other)?;
    let at = libc::IFNAMSIZ;
    let layout = || io::Error::other("a struct ifreq too small for its fields");
    ifreq.get_mut(..TAP.len()).ok_or_else(layout)?.copy_from_slice(TAP.as_bytes());
    ifreq.get_mut(at..at + 2).ok_or_else(layout)?.copy_from_slice(&short.to_ne_bytes());
    ioctl_with(&tun, libc::TUNSETIFF, &mut ifreq)?;
    ioctl_value(&tun, libc::TUNSETOWNER, libc::c_ulong::from(owner))?;
    ioctl_value(&tun, libc::TUNSETPERSIST, 1)
}

/// Writes `value` to the sysctl at `path` beneath `/proc/sys/net`, which is the calling
/// thread's namespace's own.
fn sysctl(path: &str, value: &str) -> io::Result<()> {
    std::fs::write(format!("/proc/sys/net/{path}"), value)
        .map_err(|e| io::Error::new(e.kind(), format!("net/{path}: {e}")))
}

/// What a microVM's namespace is configured with: the workload's addresses, the hardware
/// address of the host's side of the link, and the id that owns the tap.
pub(super) struct Routed {
    pub(super) ipv4: Ipv4Addr,
    pub(super) ipv6: Option<Ipv6Addr>,
    pub(super) gateway_mac: Vec<u8>,
    pub(super) owner: u32,
}

/// Configures the routing between `eth0` and the tap, from inside the namespace; the tap is made
/// if it is missing, and one there is left, since a running guest holds it open.
pub(super) fn route(nl: &mut Rtnl, r: &Routed) -> io::Result<()> {
    let missing = || io::Error::other("the namespace lacks its link");
    sysctl("ipv4/ip_forward", "1")?;
    if nl.link_index(TAP)?.is_none() {
        make(r.owner)?;
    }
    let (eth, tap) =
        (nl.link_index(INNER)?.ok_or_else(missing)?, nl.link_index(TAP)?.ok_or_else(missing)?);
    for link in [INNER, TAP] {
        sysctl(&format!("ipv4/conf/{link}/proxy_arp"), "1")?;
        sysctl(&format!("ipv4/neigh/{link}/proxy_delay"), "0")?;
    }
    nl.set_neighbour(eth, IpAddr::V4(GATEWAY_V4), Some(&r.gateway_mac))?;
    nl.set_up(eth)?;
    nl.set_up(tap)?;
    nl.add_route((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0), eth, Some(IpAddr::V4(GATEWAY_V4)))?;
    nl.add_route((IpAddr::V4(r.ipv4), 32), tap, None)?;
    let Some(v6) = r.ipv6 else { return Ok(()) };
    sysctl("ipv6/conf/all/forwarding", "1")?;
    for link in [INNER, TAP] {
        sysctl(&format!("ipv6/conf/{link}/proxy_ndp"), "1")?;
    }
    nl.set_neighbour(eth, IpAddr::V6(GATEWAY_V6), Some(&r.gateway_mac))?;
    nl.set_neighbour(eth, IpAddr::V6(v6), None)?;
    nl.set_neighbour(tap, IpAddr::V6(GATEWAY_V6), None)?;
    nl.add_route((IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0), eth, Some(IpAddr::V6(GATEWAY_V6)))?;
    nl.add_route((IpAddr::V6(v6), 128), tap, None)
}
