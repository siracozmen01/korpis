// SPDX-License-Identifier: AGPL-3.0-or-later

//! What every workload's network on a node has in common (§4.1 and §5.1 of
//! docs/07-networking.md), which the helper that makes it and the supervisor that serves it must
//! agree on.

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
