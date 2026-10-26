// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's network, as the supervisor asks for it (§4.1 of docs/07-networking.md): a
//! namespace of its own, bound under the helper's network root, joined to the host by a routed
//! veth pair with no bridge, its addresses and routes on both ends (or, for a microVM, routed on
//! to the tap its guest holds), and its firewall. The
//! supervisor chooses the addresses; the helper checks each against the pool it is said to be in,
//! derives every name from the workload's identifier, and makes the rest so, whatever part of it
//! a crash or a reboot left behind.

mod account;
pub(crate) mod announce;
mod asked;
mod bandwidth;
pub(crate) mod conntrack;
mod dedicated;
mod egress;
mod forward;
mod inward;
mod netns;
pub(crate) mod nft;
mod policy;
mod resolver;
pub(crate) mod rtnl;
mod shape;
mod tap;

use crate::error::{HelperError, HelperResult, identifier};
use asked::{Asked, Prefix, asked, port, range};
use korpis_proto::korpis::helper::v1 as helper;
use rtnl::Rtnl;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::AsRawFd as _;
use std::path::PathBuf;

pub use korpis_core::workload::network::{GATEWAY_V4, GATEWAY_V6, host_link, host_mac};
use korpis_core::workload::network::{RESOLVER_V4, TAP};
/// The name of the workload's side of its link.
const INNER: &str = "eth0";

/// Workload networks, their namespaces bound under `root`.
#[derive(Debug, Clone)]
pub struct Network {
    root: PathBuf,
}

fn sysctl_on(path: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|v| v.trim() == "1")
}

/// Configures the workload's side, from inside its namespace: its `eth0` holds its addresses,
/// or, for a microVM, the guest behind its tap does, the host side's hardware address `mac`.
fn inside(a: &Asked, ipv6: Option<Ipv6Addr>, mac: Vec<u8>) -> std::io::Result<()> {
    let mut nl = Rtnl::open()?;
    let missing = || std::io::Error::other("the namespace lacks its link");
    let lo = nl.link_index("lo")?.ok_or_else(missing)?;
    nl.set_up(lo)?;
    nl.add_address(lo, IpAddr::V4(RESOLVER_V4), 32)?;
    if a.tap {
        let routed = tap::Routed { ipv4: a.ipv4, ipv6, gateway_mac: mac, owner: a.ids.0 };
        tap::route(&mut nl, &routed)?;
        let own: Vec<IpAddr> =
            std::iter::once(IpAddr::V4(a.ipv4)).chain(ipv6.map(IpAddr::V6)).collect();
        let guest = nft::Guest { tap: TAP, addresses: &own };
        return nft::apply(&nft::namespace(&a.inbound, Some(&guest)));
    }
    let eth = nl.link_index(INNER)?.ok_or_else(missing)?;
    nl.add_address(eth, IpAddr::V4(a.ipv4), 32)?;
    nl.set_up(eth)?;
    nl.add_route((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0), eth, Some(IpAddr::V4(GATEWAY_V4)))?;
    if let Some(v6) = ipv6 {
        nl.add_address(eth, IpAddr::V6(v6), 128)?;
        nl.add_route((IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0), eth, Some(IpAddr::V6(GATEWAY_V6)))?;
    }
    nft::apply(&nft::namespace(&a.inbound, None))
}

/// Makes the host side of the workload's link, `link`, to its namespace in `dir`, with the
/// gateway's addresses and the routes to the workload's; a link left from a namespace `fresh`
/// replaced is made again, with the hardware address `mac`. The address it has.
fn linked(
    dir: &std::path::Path,
    (link, mac): (&str, [u8; 6]),
    fresh: bool,
    (ipv4, ipv6): (Ipv4Addr, Option<Ipv6Addr>),
) -> HelperResult<Vec<u8>> {
    let mut nl = Rtnl::open()?;
    if fresh && let Some(stale) = nl.link_index(link)? {
        nl.delete_link(stale)?;
    }
    if nl.link_index(link)?.is_none() {
        let ns = std::fs::File::open(netns::net(dir))?;
        nl.add_veth((link, mac), INNER, ns.as_raw_fd())?;
    }
    let index =
        nl.link_index(link)?.ok_or_else(|| HelperError::Failed("the link vanished".into()))?;
    nl.add_address(index, IpAddr::V4(GATEWAY_V4), 32)?;
    nl.set_up(index)?;
    nl.add_route((IpAddr::V4(ipv4), 32), index, None)?;
    if let Some(v6) = ipv6 {
        nl.add_address(index, IpAddr::V6(GATEWAY_V6), 64)?;
        nl.add_route((IpAddr::V6(v6), 128), index, None)?;
    }
    Ok(nl.link_address(index)?)
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
        let shapes = shape::offered();
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
            syn_cookies: std::fs::read_to_string("/proc/sys/net/ipv4/tcp_syncookies")
                .is_ok_and(|v| v.trim() != "0"),
            shapes: shapes.is_ok(),
            why_not_shapes: shapes.err().unwrap_or_default(),
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
        let a = Asked { shaping: bandwidth::here(a.shaping, offered.shapes, &a.workload), ..a };
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
        let fresh = netns::ensure(&dir, a.ids, a.tap)?;
        let link = host_link(&a.workload);
        let mac = linked(&dir, (&link, host_mac(&a.workload)), fresh, (a.ipv4, ipv6))?;
        let listed = forward::listed()?;
        let mut host = nft::host(&listed.chains);
        host.extend(forward::hold(&link, (a.ipv4, ipv6), &a.forwards, &listed.ports));
        let own = egress::Own { opened: &a.opened, rules: &a.rules, limits: a.limits };
        host.extend(egress::hold(&link, &own, listed.outward.contains(&link)));
        host.extend(inward::hold(&link, &a.inbound, listed.inward.contains(&link)));
        host.extend(dedicated::ensure((&link, a.ipv4), a.dedicated.as_ref(), &listed.dedicated)?);
        let class = bandwidth::class(&link, &a.shaping, &listed.account)?;
        let own = account::Own { ports: &a.inbound, class, cut_off: a.shaping.cut_off };
        host.extend(account::held(&link, &own, &listed.account));
        nft::apply(&serde_json::json!({"nftables": host}))?;
        if class.is_none() {
            bandwidth::unclass(&link, &listed.account)?;
        }
        netns::within(&netns::net(&dir), || inside(&a, ipv6, mac))?;
        bandwidth::apply((&dir, &link), &a.shaping, class, &a.inbound)?;
        resolver::write(&dir)?;
        Ok(helper::EnsureNetworkResponse {
            namespaces: dir.to_string_lossy().into_owned(),
            host_link: link,
            ipv6: ipv6.is_some(),
            ..Default::default()
        })
    }

    /// Makes the node's uplinks those `r` names (§7 of docs/07-networking.md).
    pub fn ensure_uplinks(
        &self,
        r: &helper::EnsureUplinksRequest,
    ) -> HelperResult<helper::EnsureUplinksResponse> {
        bandwidth::uplinks(r)
    }

    /// What the host's table has counted of every workload's link, in one listing of it.
    pub fn read_counters(&self) -> HelperResult<helper::ReadCountersResponse> {
        let counters = account::read(&forward::listed()?.account);
        Ok(helper::ReadCountersResponse { counters, ..Default::default() })
    }

    /// The sockets of `workload`'s resolver, opened inside its network namespace: UDP, then TCP
    /// listening. Its network must have been made.
    pub fn open_resolver(&self, workload: &str) -> HelperResult<Vec<rustix::fd::OwnedFd>> {
        self.opened(workload, resolver::sockets)
    }

    /// A socket for probing `workload` (§5 of docs/04-runtimes.md), a datagram socket if
    /// `datagram` and a stream otherwise, opened inside its network namespace, neither bound nor
    /// connected. Its network must have been made.
    pub fn open_probe(
        &self,
        workload: &str,
        datagram: bool,
    ) -> HelperResult<Vec<rustix::fd::OwnedFd>> {
        use rustix::net::{AddressFamily, SocketFlags, SocketType};
        let kind = if datagram { SocketType::DGRAM } else { SocketType::STREAM };
        self.opened(workload, || {
            let flags = SocketFlags::CLOEXEC;
            Ok(vec![rustix::net::socket_with(AddressFamily::INET, kind, flags, None)?])
        })
    }

    /// What `open` opens inside `workload`'s network namespace, which must have been made.
    fn opened(
        &self,
        workload: &str,
        open: impl FnOnce() -> std::io::Result<Vec<rustix::fd::OwnedFd>>,
    ) -> HelperResult<Vec<rustix::fd::OwnedFd>> {
        identifier("workload", workload)?;
        let net = netns::net(&self.path(workload));
        if !net.exists() {
            return Err(HelperError::Refused("the workload has no network here".into()));
        }
        Ok(netns::within(&net, open)?)
    }

    /// Removes `workload`'s network; whether there was one.
    pub fn release(&self, workload: &str) -> HelperResult<bool> {
        identifier("workload", workload)?;
        let link = host_link(workload);
        let listed = forward::listed()?;
        let mut unforwarded = forward::release(&link, &listed);
        let chained = listed.chains.contains(&egress::chain_of(&link));
        unforwarded.extend(egress::release(&link, (listed.outward.contains(&link), chained)));
        let chained = listed.chains.contains(&inward::chain_of(&link));
        unforwarded.extend(inward::release(&link, (listed.inward.contains(&link), chained)));
        let chained = listed.chains.contains(&dedicated::chain_of(&link));
        unforwarded.extend(dedicated::unheld(&link, &listed.dedicated, chained)?);
        bandwidth::unclass(&link, &listed.account)?;
        unforwarded.extend(account::release(&link, &listed.account));
        if !unforwarded.is_empty() {
            nft::apply(&serde_json::json!({"nftables": unforwarded}))?;
        }
        let mut nl = Rtnl::open()?;
        let link = match nl.link_index(&host_link(workload))? {
            Some(index) => nl.delete_link(index)?,
            None => false,
        };
        let namespace = netns::remove(&self.path(workload))?;
        Ok(link || namespace)
    }

    /// Releases every network bound under the root, as a machine is emptied of what its node
    /// made when the node did not destroy its workloads itself (§3 of docs/18-operations.md):
    /// the workloads whose networks were released, and those that could not be, with why.
    #[must_use]
    pub fn release_all(&self) -> (Vec<String>, Vec<(String, HelperError)>) {
        let mut names: Vec<String> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let (mut released, mut failed) = (Vec::new(), Vec::new());
        for name in names {
            match self.release(&name) {
                Ok(_) => released.push(name),
                Err(e) => failed.push((name, e)),
            }
        }
        (released, failed)
    }
}

#[cfg(test)]
mod tests;
