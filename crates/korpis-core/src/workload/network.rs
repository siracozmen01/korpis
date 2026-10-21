// SPDX-License-Identifier: AGPL-3.0-or-later

//! What every workload's network on a node has in common (§4.1 and §5.1 of
//! docs/07-networking.md), which the helper that makes it, the supervisor that serves it, and the
//! meter that reads its counters (§2 of docs/15-observability.md) must agree on.

use std::net::{Ipv4Addr, Ipv6Addr};

/// The address every workload's default route goes through, answered by the host's side of its
/// link.
pub const GATEWAY_V4: Ipv4Addr = Ipv4Addr::new(169, 254, 1, 1);
/// Every workload's resolver: an address of its own namespace's loopback, which its
/// `/etc/resolv.conf` names alone, answered on port 53 by sockets of that namespace its node's
/// supervisor holds, so a query never leaves the namespace.
pub const RESOLVER_V4: Ipv4Addr = Ipv4Addr::new(169, 254, 1, 53);
/// The same, for IPv6.
pub const GATEWAY_V6: Ipv6Addr = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1);
/// Queries a second each workload's resolver answers, beyond which it refuses them.
pub const RESOLVER_RATE: u32 = 50;
/// The burst above that rate.
pub const RESOLVER_BURST: u32 = 200;
/// What the host side of every workload's link is named with, and no other interface.
pub const HOST_LINK_PREFIX: &str = "kw";

/// The host side of `workload`'s link: `kw` and 13 hex digits of a 64-bit FNV-1a hash of its
/// identifier, inside the 15 bytes an interface name may have. Two workloads on one node share a
/// name with a chance of about one in 2^52 per pair.
#[must_use]
pub fn host_link(workload: &str) -> String {
    let hash = workload
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("{HOST_LINK_PREFIX}{:013x}", hash >> 12)
}
