// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's network, as the supervisor asks for it (§4.1 of docs/07-networking.md): a
//! namespace of its own, bound under the helper's network root, joined to the host by a routed
//! veth pair with no bridge, its addresses and routes on both ends, and its firewall. The
//! supervisor chooses the addresses; the helper checks each against the pool it is said to be in,
//! derives every name from the workload's identifier, and makes the rest so, whatever part of it
//! a crash or a reboot left behind.

mod account;
mod bandwidth;
mod egress;
mod forward;
mod netns;
pub(crate) mod nft;
mod policy;
mod resolver;
mod rtnl;
mod shape;

use crate::error::{HelperError, HelperResult, identifier};
use forward::{Forward, Transport};
use korpis_proto::korpis::helper::v1 as helper;
use rtnl::Rtnl;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::AsRawFd as _;
use std::path::PathBuf;

use korpis_core::workload::network::RESOLVER_V4;
pub use korpis_core::workload::network::{GATEWAY_V4, GATEWAY_V6, host_link};
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
    inbound: Vec<policy::Inbound>,
    forwards: Vec<Forward>,
    opened: Vec<egress::Opened>,
    rules: Vec<policy::Rule>,
    limits: policy::Limits,
    shaping: bandwidth::Shaping,
}

fn transport(t: buffa::EnumValue<helper::Transport>) -> HelperResult<Transport> {
    match t.as_known() {
        Some(helper::Transport::TRANSPORT_TCP) => Ok(Transport::Tcp),
        Some(helper::Transport::TRANSPORT_UDP) => Ok(Transport::Udp),
        _ => Err(HelperError::Refused("a port's transport is tcp or udp".into())),
    }
}

fn port(what: &str, p: u32, lowest: u16) -> HelperResult<u16> {
    u16::try_from(p)
        .ok()
        .filter(|p| *p >= lowest)
        .ok_or_else(|| HelperError::Refused(format!("{what} {p} is not a port it may use")))
}

/// The ports a workload accepts connections on, and those of the node forwarded to it.
type Ports = (Vec<policy::Inbound>, Vec<Forward>);

/// The ports a request accepts and forwards, checked: a node's port is never below 1024, which
/// the node's own services hold, and none is forwarded twice.
fn ports(r: &helper::EnsureNetworkRequest) -> HelperResult<Ports> {
    let inbound = policy::inbound(&r.inbound)?;
    let forwards = r
        .forwards
        .iter()
        .map(|f| {
            Ok(Forward {
                transport: transport(f.transport)?,
                node_port: port("a node's port", f.node_port, 1024)?,
                container_port: port("a port inside", f.container_port, 1)?,
            })
        })
        .collect::<HelperResult<Vec<_>>>()?;
    let mut node_ports: Vec<u16> = forwards.iter().map(|f| f.node_port).collect();
    node_ports.sort_unstable();
    node_ports.dedup();
    if node_ports.len() != forwards.len() {
        return Err(HelperError::Refused("a node's port is forwarded twice".into()));
    }
    Ok((inbound, forwards))
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
    let (inbound, forwards) = ports(r)?;
    let opened = r
        .opened
        .iter()
        .map(|d| {
            let addr = d.address.parse().map_err(|_| refuse("an opened address is not one"))?;
            let why = match d.reason.as_known() {
                Some(helper::DestinationReason::DESTINATION_REASON_DEPENDENCY) => "dependency",
                _ => return Err(refuse("an opened destination says why it is opened")),
            };
            let (port, transport) = (port("an opened port", d.port, 1)?, transport(d.transport)?);
            Ok(egress::Opened { addr, port, transport, why })
        })
        .collect::<HelperResult<Vec<_>>>()?;
    let ids = (r.uid_base, r.uid_count);
    let (rules, limits) = (policy::rules(&r.egress)?, policy::limits(r)?);
    let shaping = bandwidth::asked(r.bandwidth.as_option())?;
    let workload = r.workload.clone();
    Ok(Asked { workload, ipv4, ipv6, ids, inbound, forwards, opened, rules, limits, shaping })
}

/// Configures the workload's side, from inside its namespace.
fn inside(
    ipv4: Ipv4Addr,
    ipv6: Option<Ipv6Addr>,
    inbound: &[policy::Inbound],
) -> std::io::Result<()> {
    let mut nl = Rtnl::open()?;
    let missing = || std::io::Error::other("the namespace lacks its link");
    let lo = nl.link_index("lo")?.ok_or_else(missing)?;
    nl.set_up(lo)?;
    nl.add_address(lo, IpAddr::V4(RESOLVER_V4), 32)?;
    let eth = nl.link_index(INNER)?.ok_or_else(missing)?;
    nl.add_address(eth, IpAddr::V4(ipv4), 32)?;
    nl.set_up(eth)?;
    nl.add_route((IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0), eth, Some(IpAddr::V4(GATEWAY_V4)))?;
    if let Some(v6) = ipv6 {
        nl.add_address(eth, IpAddr::V6(v6), 128)?;
        nl.add_route((IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0), eth, Some(IpAddr::V6(GATEWAY_V6)))?;
    }
    nft::apply(&nft::namespace(inbound))
}

/// Makes the host side of the workload's link, `link`, to its namespace in `dir`, with the
/// gateway's addresses and the routes to the workload's; a link left from a namespace `fresh`
/// replaced is made again.
fn linked(
    dir: &std::path::Path,
    link: &str,
    fresh: bool,
    (ipv4, ipv6): (Ipv4Addr, Option<Ipv6Addr>),
) -> HelperResult<()> {
    let mut nl = Rtnl::open()?;
    if fresh && let Some(stale) = nl.link_index(link)? {
        nl.delete_link(stale)?;
    }
    if nl.link_index(link)?.is_none() {
        let ns = std::fs::File::open(netns::net(dir))?;
        nl.add_veth(link, INNER, ns.as_raw_fd())?;
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
    Ok(())
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
        linked(&dir, &link, fresh, (a.ipv4, ipv6))?;
        let listed = forward::listed()?;
        let mut host = nft::host(&listed.chains);
        host.extend(forward::hold(&link, (a.ipv4, ipv6), &a.forwards, &listed.ports));
        let own = egress::Own { opened: &a.opened, rules: &a.rules, limits: a.limits };
        host.extend(egress::hold(&link, &own, listed.outward.contains(&link)));
        let class = bandwidth::class(&link, &a.shaping, &listed.account)?;
        let own = account::Own { ports: &a.inbound, class, cut_off: a.shaping.cut_off };
        host.extend(account::held(&link, &own, &listed.account));
        nft::apply(&serde_json::json!({"nftables": host}))?;
        if class.is_none() {
            bandwidth::unclass(&link, &listed.account)?;
        }
        netns::within(&netns::net(&dir), || inside(a.ipv4, ipv6, &a.inbound))?;
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

    /// The sockets of `workload`'s resolver, opened inside its network namespace: UDP, then TCP
    /// listening. Its network must have been made.
    pub fn open_resolver(&self, workload: &str) -> HelperResult<Vec<rustix::fd::OwnedFd>> {
        identifier("workload", workload)?;
        let net = netns::net(&self.path(workload));
        if !net.exists() {
            return Err(HelperError::Refused("the workload has no network here".into()));
        }
        Ok(netns::within(&net, resolver::sockets)?)
    }

    /// Removes `workload`'s network; whether there was one.
    pub fn release(&self, workload: &str) -> HelperResult<bool> {
        identifier("workload", workload)?;
        let link = host_link(workload);
        let listed = forward::listed()?;
        let mut unforwarded = forward::release(&link, &listed);
        let chained = listed.chains.contains(&egress::chain_of(&link));
        unforwarded.extend(egress::release(&link, (listed.outward.contains(&link), chained)));
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
