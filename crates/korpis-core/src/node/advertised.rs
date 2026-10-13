// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node offers for inbound traffic before anything has measured it (§9 of
//! docs/07-networking.md): the addresses clients would reach it at, and the ports its endpoints
//! are allocated from. Reachability is measured against these, never assumed from them.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The inclusive range of ports a node allocates `node_port` and `node_bound` endpoints from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortRange {
    first: u16,
    last: u16,
}

/// Why a port range was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PortRangeError {
    /// It is empty or reversed.
    #[error("the first port {first} is above the last {last}")]
    Reversed {
        /// The first.
        first: u16,
        /// The last.
        last: u16,
    },
    /// It reaches the privileged ports, which a workload's endpoint must not take from the
    /// machine's own services.
    #[error("port {0} is privileged; the range starts at 1024 or above")]
    Privileged(u16),
}

impl PortRange {
    /// The default: below Linux's ephemeral range (32768 and up), so an allocated port never
    /// collides with an outgoing connection's, and above the ports services conventionally
    /// listen on. Kubernetes chose the same range for the same reasons.
    pub const DEFAULT: Self = Self { first: 30_000, last: 32_767 };

    /// `first..=last`, if it is a usable range.
    pub fn new(first: u16, last: u16) -> Result<Self, PortRangeError> {
        if first > last {
            return Err(PortRangeError::Reversed { first, last });
        }
        if first < 1024 {
            return Err(PortRangeError::Privileged(first));
        }
        Ok(Self { first, last })
    }

    /// The first port.
    #[must_use]
    pub fn first(self) -> u16 {
        self.first
    }

    /// The last port.
    #[must_use]
    pub fn last(self) -> u16 {
        self.last
    }

    /// Whether `port` is in it.
    #[must_use]
    pub fn contains(self, port: u16) -> bool {
        (self.first..=self.last).contains(&port)
    }
}

impl std::str::FromStr for PortRange {
    type Err = String;

    /// `30000-32767`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (first, last) = s.split_once('-').ok_or("expected FIRST-LAST")?;
        let port = |p: &str| p.trim().parse::<u16>().map_err(|e| format!("{p}: {e}"));
        Self::new(port(first)?, port(last)?).map_err(|e| e.to_string())
    }
}

/// What a node advertises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertised {
    /// Where clients would reach it.
    pub addresses: Vec<IpAddr>,
    /// Where its endpoints' ports come from.
    pub ports: PortRange,
}

/// Whether `ip` is routable on the internet: not private, shared, loopback, link-local,
/// multicast, reserved, or set aside for documentation. A node advertises such an address of its
/// own accord; any other only when its operator says so.
#[must_use]
pub fn is_global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_global_v4(v4),
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or_else(|| is_global_v6(v6), is_global_v4),
    }
}

fn is_global_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let shared = a == 100 && (64..128).contains(&b);
    let benchmarking = a == 198 && (b == 18 || b == 19);
    let documentation = matches!((a, b, c), (192, 0, 2) | (198, 51, 100) | (203, 0, 113));
    let protocol = a == 192 && b == 0 && c == 0;
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_unspecified()
        || a == 0
        || a >= 240
        || shared
        || benchmarking
        || documentation
        || protocol)
}

fn is_global_v6(ip: Ipv6Addr) -> bool {
    let [first, second, ..] = ip.segments();
    // Global unicast is 2000::/3; within it, documentation is 2001:db8::/32.
    (first & 0xe000) == 0x2000 && !(first == 0x2001 && second == 0x0db8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_ordered_and_unprivileged() {
        assert_eq!("30000-32767".parse::<PortRange>(), Ok(PortRange::DEFAULT));
        assert!("32767-30000".parse::<PortRange>().is_err());
        assert!("80-90".parse::<PortRange>().is_err());
        assert!("25565-25565".parse::<PortRange>().unwrap().contains(25_565));
        assert!(!PortRange::DEFAULT.contains(32_768), "the ephemeral range starts there");
    }

    #[test]
    fn only_internet_routable_addresses_are_global() {
        let global = |s: &str| is_global(s.parse().unwrap());
        for g in ["8.8.8.8", "2a01:4f8::1", "::ffff:8.8.8.8"] {
            assert!(global(g), "{g}");
        }
        let local = [
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "0.0.0.0",
            "192.0.2.1",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "2001:db8::1",
            "::ffff:10.0.0.1",
            "ff02::1",
        ];
        for l in local {
            assert!(!global(l), "{l}");
        }
    }
}
