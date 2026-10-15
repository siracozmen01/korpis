// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's network, as the supervisor asks for it (§4.1 of docs/07-networking.md): a
//! namespace of its own, bound under the helper's network root, joined to the host by a routed
//! veth pair with no bridge, its addresses and routes on both ends, and its firewall. The
//! supervisor chooses the addresses; the helper checks each against the pool it is said to be in,
//! derives every name from the workload's identifier, and makes the rest so, whatever part of it
//! a crash or a reboot left behind.

mod netns;
mod nft;
mod rtnl;

use crate::error::{HelperError, HelperResult, identifier};
use korpis_proto::korpis::helper::v1 as helper;
use rtnl::Rtnl;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::AsRawFd as _;
use std::path::PathBuf;

/// The address every workload's default route goes through, answered by the host's side of its
/// link (§4.1 of docs/07-networking.md).
pub const GATEWAY_V4: Ipv4Addr = Ipv4Addr::new(169, 254, 1, 1);
/// The same, for IPv6.
pub const GATEWAY_V6: Ipv6Addr = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1);
/// The name of the workload's side of its link.
const INNER: &str = "eth0";

/// A network prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Prefix {
    addr: IpAddr,
    len: u8,
}

/// Workload networks, their namespaces bound under `root`.
#[derive(Debug, Clone)]
pub struct Network {
    root: PathBuf,
}

fn sysctl_on(path: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|v| v.trim() == "1")
}

/// The host side of `workload`'s link: `kw` and 13 hex digits of a 64-bit FNV-1a hash of its
/// identifier, inside the 15 bytes an interface name may have. Two workloads on one node share a
/// name with a chance of about one in 2^52 per pair.
#[must_use]
pub fn host_link(workload: &str) -> String {
    let hash = workload
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("kw{:013x}", hash >> 12)
}

fn prefix(what: &str, text: &str) -> HelperResult<Prefix> {
    let bad = || HelperError::Refused(format!("{what} is not a prefix"));
    let (addr, len) = text.split_once('/').ok_or_else(bad)?;
    let addr: IpAddr = addr.parse().map_err(|_| bad())?;
    let len: u8 = len.parse().map_err(|_| bad())?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    if len > max || len == 0 {
        return Err(bad());
    }
    Ok(Prefix { addr, len })
}

fn bits(ip: IpAddr) -> (u128, u8) {
    match ip {
        IpAddr::V4(v4) => (u128::from(u32::from(v4)), 32),
        IpAddr::V6(v6) => (u128::from(v6), 128),
    }
}

/// Whether `ip` is in `pool` and is neither its first address nor its last.
fn within(ip: IpAddr, pool: Prefix) -> bool {
    let ((a, width), (p, pool_width)) = (bits(ip), bits(pool.addr));
    if width != pool_width {
        return false;
    }
    let host_bits = u32::from(width - pool.len);
    let mask =
        u128::MAX.checked_shl(host_bits).unwrap_or(0) & (u128::MAX >> (128 - u32::from(width)));
    let host = a & !mask;
    let last = !mask & (u128::MAX >> (128 - u32::from(width)));
    a & mask == p & mask && host != 0 && host != last
}

/// What a request asks for, checked.
struct Asked {
    workload: String,
    ipv4: Ipv4Addr,
    ipv6: Option<Ipv6Addr>,
    ids: (u32, u32),
}

fn asked(r: &helper::EnsureNetworkRequest) -> HelperResult<Asked> {
    identifier("workload", &r.workload)?;
    let refuse = |m: &str| HelperError::Refused(m.to_owned());
    let ipv4: Ipv4Addr = r.ipv4.parse().map_err(|_| refuse("ipv4 is not an address"))?;
    let ipv4_pool = prefix("ipv4_pool", &r.ipv4_pool)?;
    if !within(IpAddr::V4(ipv4), ipv4_pool) {
        return Err(refuse("ipv4 is not a host address of its pool"));
    }
    let ipv6 = if r.ipv6.is_empty() {
        None
    } else {
        let ip: Ipv6Addr = r.ipv6.parse().map_err(|_| refuse("ipv6 is not an address"))?;
        let pool = prefix("ipv6_pool", &r.ipv6_pool)?;
        if !within(IpAddr::V6(ip), pool) {
            return Err(refuse("ipv6 is not a host address of its pool"));
        }
        Some(ip)
    };
    if r.uid_count == 0 || r.uid_base.checked_add(r.uid_count).is_none() {
        return Err(refuse("the workload holds no uid range for its namespaces to map"));
    }
    Ok(Asked { workload: r.workload.clone(), ipv4, ipv6, ids: (r.uid_base, r.uid_count) })
}

/// Configures the workload's side, on a thread inside its namespace.
fn inside(ipv4: Ipv4Addr, ipv6: Option<Ipv6Addr>) -> std::io::Result<()> {
    let mut nl = Rtnl::open()?;
    let missing = || std::io::Error::other("the namespace lacks its link");
    let lo = nl.link_index("lo")?.ok_or_else(missing)?;
    nl.set_up(lo)?;
    let eth = nl.link_index(INNER)?.ok_or_else(missing)?;
    nl.add_address(eth, IpAddr::V4(ipv4), 32)?;
    nl.set_up(eth)?;
    nl.add_route((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0), eth, Some(IpAddr::V4(GATEWAY_V4)))?;
    if let Some(v6) = ipv6 {
        nl.add_address(eth, IpAddr::V6(v6), 128)?;
        nl.add_route((IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0), eth, Some(IpAddr::V6(GATEWAY_V6)))?;
    }
    nft::apply(&nft::namespace())
}

impl Network {
    /// Networks whose namespaces are bound under `root`.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, workload: &str) -> PathBuf {
        self.root.join(workload)
    }

    /// What this node can offer, and what stands in its way.
    #[must_use]
    pub fn describe(&self) -> helper::DescribeNetworkResponse {
        let closed = nft::closed_forward();
        let why_not = if !sysctl_on("/proc/sys/net/ipv4/ip_forward") {
            "IPv4 forwarding is off; `sysctl -w net.ipv4.ip_forward=1` turns it on".to_owned()
        } else if let Err(e) = &closed {
            format!("nftables did not answer: {e}")
        } else {
            String::new()
        };
        helper::DescribeNetworkResponse {
            offered: why_not.is_empty(),
            why_not,
            ipv6: sysctl_on("/proc/sys/net/ipv6/conf/all/forwarding"),
            closed_forward: closed.unwrap_or_default(),
            ..Default::default()
        }
    }

    /// Makes `r`'s network so.
    pub fn ensure(
        &self,
        r: &helper::EnsureNetworkRequest,
    ) -> HelperResult<helper::EnsureNetworkResponse> {
        let a = asked(r)?;
        let offered = self.describe();
        if !offered.offered {
            return Err(HelperError::Failed(offered.why_not));
        }
        let ipv6 = a.ipv6.filter(|_| offered.ipv6);
        if a.ipv6.is_some() && ipv6.is_none() {
            tracing::warn!(
                workload = %a.workload,
                "IPv6 forwarding is off, so the workload has IPv4 alone; \
                 `sysctl -w net.ipv6.conf.all.forwarding=1` turns it on, and stops the host \
                 taking router advertisements on its own interfaces"
            );
        }
        let dir = self.path(&a.workload);
        let fresh = netns::ensure(&dir, a.ids)?;
        let link = host_link(&a.workload);
        let mut nl = Rtnl::open()?;
        if fresh && let Some(stale) = nl.link_index(&link)? {
            nl.delete_link(stale)?;
        }
        if nl.link_index(&link)?.is_none() {
            let ns = std::fs::File::open(netns::net(&dir))?;
            nl.add_veth(&link, INNER, ns.as_raw_fd())?;
        }
        let index =
            nl.link_index(&link)?.ok_or_else(|| HelperError::Failed("the link vanished".into()))?;
        nl.add_address(index, IpAddr::V4(GATEWAY_V4), 32)?;
        nl.set_up(index)?;
        nl.add_route((IpAddr::V4(a.ipv4), 32), index, None)?;
        if let Some(v6) = ipv6 {
            nl.add_address(index, IpAddr::V6(GATEWAY_V6), 64)?;
            nl.add_route((IpAddr::V6(v6), 128), index, None)?;
        }
        nft::apply(&nft::host())?;
        netns::within(&netns::net(&dir), || inside(a.ipv4, ipv6))?;
        Ok(helper::EnsureNetworkResponse {
            namespaces: dir.to_string_lossy().into_owned(),
            host_link: link,
            ipv6: ipv6.is_some(),
            ..Default::default()
        })
    }

    /// Removes `workload`'s network; whether there was one.
    pub fn release(&self, workload: &str) -> HelperResult<bool> {
        identifier("workload", workload)?;
        let mut nl = Rtnl::open()?;
        let link = match nl.link_index(&host_link(workload))? {
            Some(index) => nl.delete_link(index)?,
            None => false,
        };
        let namespace = netns::remove(&self.path(workload))?;
        Ok(link || namespace)
    }
}

#[cfg(test)]
mod tests;
