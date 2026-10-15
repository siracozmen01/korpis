// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workloads' addresses on this node, and whether the node gives workloads a network at all
//! (§4.1 of docs/07-networking.md). The agent allocates each workload the lowest free address of
//! each pool when it first runs here, keeps it on disk across restarts, and frees it when the
//! workload's network is released; the helper makes the network from what is allocated.

use crate::durable;
use crate::host::Host;
use crate::runtime::Runtime;
use ipnet::{Ipv4Net, Ipv6Net};
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

/// The IPv4 pool a node's workloads are given addresses from unless its operator names another.
pub const DEFAULT_IPV4: &str = "10.47.0.0/16";

/// The pools a node's workloads are given addresses from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pools {
    /// IPv4, private to the node and translated on the way out.
    pub ipv4: Ipv4Net,
    /// IPv6: a prefix routed to the node, or a unique local one derived from its identity;
    /// none where the node does not forward IPv6.
    pub ipv6: Option<Ipv6Net>,
}

/// A unique local /64 for `node`: `fd`, then 40 bits of a hash of its identifier as RFC 4193's
/// global identifier, then subnet zero. Derived rather than random so that it survives the
/// agent's state, and different for each node so two nodes' workloads never share one.
#[must_use]
pub fn unique_local(node: &str) -> Ipv6Net {
    let hash = Sha256::digest(node.as_bytes());
    let mut octets = [0_u8; 16];
    octets[0] = 0xfd;
    for (to, from) in octets.iter_mut().skip(1).zip(hash.iter().take(5)) {
        *to = *from;
    }
    Ipv6Net::new(Ipv6Addr::from(octets), 64).unwrap_or_default()
}

/// What one workload holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assigned {
    /// Its IPv4 address.
    pub ipv4: Ipv4Addr,
    /// Its IPv6 address, where the node gives one.
    pub ipv6: Option<Ipv6Addr>,
}

/// The `n`th address of a pool `width` bits wide whose first address is `base`, if it is a host
/// address: neither the first nor the last.
fn nth(base: u128, host_bits: u32, n: u128) -> Option<u128> {
    let size = 1_u128.checked_shl(host_bits)?;
    (n > 0 && n < size.saturating_sub(1)).then(|| base + n)
}

/// The lowest host address of the pool at `base` with `host_bits` that `used` lacks.
fn lowest_free(base: u128, host_bits: u32, used: &[u128]) -> Option<u128> {
    (1..).map_while(|n| nth(base, host_bits, n)).find(|a| !used.contains(a))
}

fn full() -> std::io::Error {
    std::io::Error::other("the workload address pool is full")
}

/// `held` if it is a host address of the pool at `base` with `host_bits`, otherwise the lowest
/// one `used` lacks.
fn pick(
    held: Option<u128>,
    (base, host_bits): (u128, u32),
    used: &[u128],
) -> std::io::Result<u128> {
    let size = 1_u128.checked_shl(host_bits).unwrap_or(0);
    let inside = |a: u128| a > base && a - base < size.saturating_sub(1);
    match held.filter(|a| inside(*a)) {
        Some(a) => Ok(a),
        None => lowest_free(base, host_bits, used).ok_or_else(full),
    }
}

/// The addresses allocated on this node, kept at a path one workload per line.
#[derive(Debug)]
pub struct Addresses {
    path: PathBuf,
    held: BTreeMap<String, Assigned>,
}

fn parse_line(line: &str) -> Option<(String, Assigned)> {
    let mut parts = line.split(' ');
    let workload = parts.next()?.to_owned();
    let ipv4 = parts.next()?.parse().ok()?;
    let ipv6 = parts.next().and_then(|v| v.parse().ok());
    Some((workload, Assigned { ipv4, ipv6 }))
}

impl Addresses {
    /// The addresses kept at `path`; none when nothing was kept there.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let held = match std::fs::read_to_string(path) {
            Ok(text) => text.lines().filter_map(parse_line).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path: path.to_owned(), held })
    }

    fn keep(&self, held: &BTreeMap<String, Assigned>) -> std::io::Result<()> {
        let text: String = held
            .iter()
            .map(|(w, a)| match a.ipv6 {
                Some(v6) => format!("{w} {} {v6}\n", a.ipv4),
                None => format!("{w} {}\n", a.ipv4),
            })
            .collect();
        durable::replace(&self.path, text.as_bytes())
    }

    /// What `workload` holds, if it holds addresses.
    #[must_use]
    pub fn of(&self, workload: &str) -> Option<Assigned> {
        self.held.get(workload).copied()
    }

    /// The workloads holding addresses.
    pub fn workloads(&self) -> impl Iterator<Item = &String> {
        self.held.keys()
    }

    /// What `workload` holds in `pools`: what it held if that is still in them, otherwise the
    /// lowest free addresses, which a changed pool renumbers it to.
    pub fn assign(&mut self, workload: &str, pools: Pools) -> std::io::Result<Assigned> {
        let held = self.held.get(workload).copied();
        let others: Vec<Assigned> =
            self.held.iter().filter(|(w, _)| *w != workload).map(|(_, a)| *a).collect();
        let v4 = held.map(|a| u128::from(u32::from(a.ipv4)));
        let used4: Vec<u128> = others.iter().map(|a| u128::from(u32::from(a.ipv4))).collect();
        let base4 = u128::from(u32::from(pools.ipv4.network()));
        let ipv4 = pick(v4, (base4, 32 - u32::from(pools.ipv4.prefix_len())), &used4)?;
        let ipv4 = Ipv4Addr::from(u32::try_from(ipv4).map_err(|_| full())?);
        let ipv6 = match pools.ipv6 {
            None => None,
            Some(pool) => {
                let v6 = held.and_then(|a| a.ipv6).map(u128::from);
                let used6: Vec<u128> =
                    others.iter().filter_map(|a| a.ipv6).map(u128::from).collect();
                let at = (u128::from(pool.network()), 128 - u32::from(pool.prefix_len()));
                Some(Ipv6Addr::from(pick(v6, at, &used6)?))
            }
        };
        let assigned = Assigned { ipv4, ipv6 };
        if held != Some(assigned) {
            let mut next = self.held.clone();
            next.insert(workload.to_owned(), assigned);
            self.keep(&next)?;
            self.held = next;
        }
        Ok(assigned)
    }

    /// Frees what `workload` holds.
    pub fn release(&mut self, workload: &str) -> std::io::Result<()> {
        if !self.held.contains_key(workload) {
            return Ok(());
        }
        let mut next = self.held.clone();
        next.remove(workload);
        self.keep(&next)?;
        self.held = next;
        Ok(())
    }
}

/// A node's workload networks: its pools and what it allocated from them.
#[derive(Debug)]
pub struct Networking {
    /// The pools.
    pub pools: Pools,
    /// What is allocated.
    pub addresses: Addresses,
    /// The control plane's API as this node reaches it, host and port, which an installed
    /// extension's workload is opened to (§6 of docs/07-networking.md).
    pub api: Option<(String, u16)>,
    /// The resolver this agent serves for each workload, on sockets of its namespace (§5.1
    /// there); dropping one closes them.
    pub resolvers: std::collections::BTreeMap<String, tokio::task::JoinHandle<()>>,
    /// What each workload's network opens for its dependencies, as last made (§5 there).
    pub dependencies_opened: std::collections::BTreeMap<String, Vec<helper::Destination>>,
}

/// The host and port of the URL a node enrolled with, `server`: where it reaches the control
/// plane's API. An `https` URL without a port is on 443.
#[must_use]
pub fn api_of(server: &str) -> Option<(String, u16)> {
    let uri: http::Uri = server.parse().ok()?;
    let host = uri.host()?.trim_start_matches('[').trim_end_matches(']').to_owned();
    let port = uri.port_u16().unwrap_or(if uri.scheme_str() == Some("http") { 80 } else { 443 });
    Some((host, port))
}

impl Networking {
    /// What the helper is asked to make for `workload`, holding `assigned`.
    #[must_use]
    pub fn request(&self, workload: &str, assigned: Assigned) -> helper::EnsureNetworkRequest {
        helper::EnsureNetworkRequest {
            workload: workload.to_owned(),
            ipv4: assigned.ipv4.to_string(),
            ipv4_pool: self.pools.ipv4.to_string(),
            ipv6: assigned.ipv6.map(|v| v.to_string()).unwrap_or_default(),
            ipv6_pool: self.pools.ipv6.map(|p| p.to_string()).unwrap_or_default(),
            ..Default::default()
        }
    }
}

/// Logs what another firewall closes, and how its operator opens it.
fn warn_closed(closed: &[String]) {
    for chain in closed {
        tracing::warn!(
            %chain,
            "this chain drops forwarded traffic it does not accept, so workloads reach nothing \
             beyond this node and no client reaches their endpoints until it accepts traffic on \
             Korpis's links, named kw*: `nft insert rule {chain} iifname \"kw*\" accept` and \
             `nft insert rule {chain} oifname \"kw*\" accept`, or the same through the tool \
             that manages it; Korpis does not change another firewall"
        );
    }
}

/// The pools this node's workloads get addresses from, if the node gives them networks: the
/// helper must offer them and the driver must join a namespace it is given. `ipv6` is a prefix
/// routed to the node; without one the node's own unique local prefix serves.
pub async fn offered<R: Runtime, H: Host>(
    runtime: &R,
    host: &H,
    node: &str,
    (ipv4, ipv6): (Ipv4Net, Option<Ipv6Net>),
) -> Option<Pools> {
    let joins = runtime.describe().await.is_ok_and(|d| {
        let modes = d.capabilities.as_option().map(|c| c.network_modes.clone()).unwrap_or_default();
        modes.iter().any(|m| m.as_known() == Some(pb::NetworkMode::NETWORK_MODE_VETH))
    });
    if !joins {
        tracing::info!("the driver joins no network namespace; workloads have loopback alone");
        return None;
    }
    let described = match host.describe_network().await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "the helper did not describe its network; none is offered");
            return None;
        }
    };
    if !described.offered {
        tracing::warn!(why = %described.why_not, "workloads have loopback alone");
        return None;
    }
    warn_closed(&described.closed_forward);
    if !described.syn_cookies {
        tracing::warn!(
            "the kernel does not answer SYN floods with SYN cookies, so half-open connections \
             can fill the connection table; `sysctl -w net.ipv4.tcp_syncookies=1` turns them on"
        );
    }
    let ipv6 = described.ipv6.then(|| ipv6.unwrap_or_else(|| unique_local(node)));
    Some(Pools { ipv4, ipv6 })
}

#[cfg(test)]
mod tests;
