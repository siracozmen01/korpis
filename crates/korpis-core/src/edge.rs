// SPDX-License-Identifier: AGPL-3.0-or-later

//! Edge groups (§3.1 of docs/07-networking.md, §3.6 of docs/01-model.md). A group is two or more
//! of an organization's own nodes holding the stable addresses of its `stable` endpoints, and
//! those of the organizations below it, each address on one member at a time. A `stable`
//! endpoint is given an address of one of the group's pools and a port of it, which it keeps for
//! as long as its workload declares it, whatever node the workload runs on.

use crate::address::{AddressPool, Taken};
use crate::workload::endpoint::Transport;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

pub mod lease;
pub mod probe;
pub mod table;

/// The ports a group gives `stable` endpoints unless it says otherwise.
pub const DEFAULT_PORTS: (u16, u16) = (1024, 65_534);
/// The port its members forward to a node for the control plane's probe, unless it says
/// otherwise; outside [`DEFAULT_PORTS`].
pub const DEFAULT_PROBE_PORT: u16 = 65_535;
/// How long between two probes of a member, unless the group says otherwise.
pub const DEFAULT_PROBE_INTERVAL: Duration = Duration::from_secs(1);
/// How many probes in a row a member misses before it is failing, unless the group says
/// otherwise.
pub const DEFAULT_PROBE_MISSES: u32 = 3;
/// A member's lease on an address, unless the group says otherwise.
pub const DEFAULT_TERM: Duration = Duration::from_secs(10);
/// The shortest lease a group may give: a third of it is when its holder renews, and a renewal
/// is asked again after five seconds at most (§4.5 of docs/02-architecture.md).
pub const MIN_TERM: Duration = Duration::from_secs(3);

/// An edge group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeGroup {
    /// The group.
    pub id: Uuid,
    /// The organization it belongs to, whose workloads and those below it it serves.
    pub organization: Uuid,
    /// Its name, unique within its organization.
    pub name: String,
    /// Its members, nodes of its organization; fewer than two serve nothing.
    pub members: Vec<Uuid>,
    /// The first and last port given to `stable` endpoints.
    pub ports: (u16, u16),
    /// The port every member forwards for the control plane's probe.
    pub probe_port: u16,
    /// Between two probes of a member.
    pub probe_interval: Duration,
    /// Missed in a row before a member is failing.
    pub probe_misses: u32,
    /// A member's lease on an address.
    pub term: Duration,
    /// When it was made.
    pub created_at: SystemTime,
}

/// Why a group is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GroupError {
    /// Its name is not a lowercase word.
    #[error("{0:?} is not a group's name: lowercase letters, digits, and hyphens")]
    Name(String),
    /// Its ports are reversed or start at zero.
    #[error("the ports {0} to {1} are not a range of ports")]
    Ports(u16, u16),
    /// Its probe port is zero or one of its ports.
    #[error("the probe port {0} must be a port outside the group's ports")]
    ProbePort(u16),
    /// Its probe interval is under a tenth of a second or over a minute.
    #[error("a probe interval is a tenth of a second to a minute")]
    ProbeInterval,
    /// Its misses are none or over a hundred.
    #[error("a member misses one to a hundred probes before it is failing")]
    ProbeMisses,
    /// Its term is under [`MIN_TERM`] or over ten minutes.
    #[error("a term is three seconds to ten minutes")]
    Term,
    /// A node is named twice.
    #[error("node {0} is named twice")]
    Member(Uuid),
}

impl EdgeGroup {
    /// Checks what the group itself says.
    ///
    /// # Errors
    /// The first thing it says that could not hold.
    pub fn validate(&self) -> Result<(), GroupError> {
        let n = &self.name;
        let word = !n.is_empty()
            && n.len() <= 63
            && !n.starts_with('-')
            && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !word {
            return Err(GroupError::Name(n.clone()));
        }
        let (first, last) = self.ports;
        if first == 0 || first > last {
            return Err(GroupError::Ports(first, last));
        }
        if self.probe_port == 0 || (first..=last).contains(&self.probe_port) {
            return Err(GroupError::ProbePort(self.probe_port));
        }
        let interval = Duration::from_millis(100)..=Duration::from_secs(60);
        if !interval.contains(&self.probe_interval) {
            return Err(GroupError::ProbeInterval);
        }
        if !(1..=100).contains(&self.probe_misses) {
            return Err(GroupError::ProbeMisses);
        }
        if !(MIN_TERM..=Duration::from_secs(600)).contains(&self.term) {
            return Err(GroupError::Term);
        }
        let mut seen = std::collections::BTreeSet::new();
        match self.members.iter().find(|m| !seen.insert(**m)) {
            Some(m) => Err(GroupError::Member(*m)),
            None => Ok(()),
        }
    }

    /// Whether it serves anything: an address one machine holds is `node_bound`, whatever it
    /// is called (§3.1 of docs/07-networking.md).
    #[must_use]
    pub fn serves(&self) -> bool {
        self.members.len() >= 2
    }

    /// The detection interval it declares: how long a member is failing before it is known.
    #[must_use]
    pub fn detection(&self) -> Duration {
        self.probe_interval.saturating_mul(self.probe_misses)
    }
}

/// An address and port of an edge pool a `stable` endpoint holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StablePort {
    /// The address.
    pub address: IpAddr,
    /// The port.
    pub port: u16,
    /// The transport its endpoint rides on: a port is held per transport, so a game's TCP port
    /// and its query's UDP port may share a number, as Minecraft's do.
    pub transport: Transport,
    /// The pool it is of.
    pub pool: Uuid,
    /// The workload holding it.
    pub workload: Uuid,
    /// The endpoint of that workload it is for, by name.
    pub endpoint: String,
}

/// What holds each address, port, and transport of a group's pools: a workload, or no one since
/// an instant.
pub type TakenPorts = BTreeMap<(IpAddr, u16, Transport), Taken>;

/// Whether `(address, port, transport)` may be given at `now`: no one holds it, and its rest
/// after the last holder left has passed `quarantine`.
fn free(
    taken: &TakenPorts,
    at: (IpAddr, u16, Transport),
    quarantine: Duration,
    now: SystemTime,
) -> bool {
    match taken.get(&at) {
        None => true,
        Some(Taken::Held(_)) => false,
        Some(Taken::Released(when)) => now.duration_since(*when).is_ok_and(|r| r >= quarantine),
    }
}

/// What a workload's `stable` endpoints are given: one address, and a port on it for each, in
/// the order asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Given {
    /// The address.
    pub address: IpAddr,
    /// A port for each endpoint.
    pub ports: Vec<u16>,
}

/// The address and ports of `pool` that `wanted`, each endpoint's declared container port and
/// transport, is given at `now` within `range`: on the lowest address on which every endpoint has its declared
/// port, and failing that the lowest on which each has its declared port or else the lowest
/// free port of the range; on `prefer`, the address the workload holds already, wherever it has
/// room for them.
/// None where no address has room for all of them.
#[must_use]
pub fn allocate(
    (pool, taken): (&AddressPool, &TakenPorts),
    wanted: &[(u16, Transport)],
    (range, prefer): ((u16, u16), Option<IpAddr>),
    now: SystemTime,
) -> Option<Given> {
    let fit = |address: IpAddr, declared_only: bool| -> Option<Given> {
        let mut ports: Vec<u16> = Vec::with_capacity(wanted.len());
        let mut chosen: Vec<(u16, Transport)> = Vec::with_capacity(wanted.len());
        for &(w, t) in wanted {
            let usable = |p: u16| {
                (range.0..=range.1).contains(&p)
                    && !chosen.contains(&(p, t))
                    && free(taken, (address, p, t), pool.quarantine, now)
            };
            let port = if usable(w) {
                w
            } else if declared_only {
                return None;
            } else {
                (range.0..=range.1).find(|p| usable(*p))?
            };
            ports.push(port);
            chosen.push((port, t));
        }
        Some(Given { address, ports })
    };
    // Its own address first, so a workload's endpoints share one wherever they can.
    let own = prefer.filter(|p| pool.cidr.contains(p));
    let mut on_its_own = [true, false].into_iter().filter_map(|d| own.and_then(|a| fit(a, d)));
    let mut passes = [true, false].into_iter();
    on_its_own.next().or_else(|| {
        passes.find_map(|declared_only| pool.addresses().find_map(|a| fit(a, declared_only)))
    })
}

/// The lowest address of `pool` on which every one of `ports` is free at `now`, for the IPv6
/// address a workload's `stable` endpoints are given beside their IPv4 one, on the same ports.
#[must_use]
pub fn alongside(
    (pool, taken): (&AddressPool, &TakenPorts),
    ports: &[(u16, Transport)],
    prefer: Option<IpAddr>,
    now: SystemTime,
) -> Option<IpAddr> {
    let mut addresses =
        prefer.filter(|p| pool.cidr.contains(p)).into_iter().chain(pool.addresses());
    let all_free =
        |a: IpAddr| ports.iter().all(|(p, t)| free(taken, (a, *p, *t), pool.quarantine, now));
    addresses.find(|a| all_free(*a))
}

/// The ports of a group's ingress address its members' proxies serve: plain HTTP and TLS.
pub const INGRESS_PORTS: [u16; 2] = [80, 443];

/// A group's ingress address of one family (§3.1 of docs/07-networking.md): an address of one
/// of its pools whose ports [`INGRESS_PORTS`] go, over TCP and UDP, to its holder's own proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressAddress {
    /// The address.
    pub address: IpAddr,
    /// The group.
    pub group: Uuid,
    /// The pool it is of.
    pub pool: Uuid,
}

/// The ports and transports a group's ingress address keeps from `stable` endpoints.
#[must_use]
pub fn ingress_ports() -> [(u16, Transport); 4] {
    let [http, tls] = INGRESS_PORTS;
    [(http, Transport::Tcp), (tls, Transport::Tcp), (http, Transport::Udp), (tls, Transport::Udp)]
}

/// The address of `pool` a group's ingress address is chosen at, at `now`: its lowest on which
/// [`ingress_ports`] are all free, so it may also be a `stable` endpoint's on other ports.
#[must_use]
pub fn ingress_room((pool, taken): (&AddressPool, &TakenPorts), now: SystemTime) -> Option<IpAddr> {
    alongside((pool, taken), &ingress_ports(), None, now)
}

/// A node's own address in the mesh: the last of its IPv4 pool, which no workload is ever given
/// (§4.1 of docs/07-networking.md), from which its GRE links to and from edges are made.
#[must_use]
pub fn mesh_address(pools: &[ipnet::IpNet]) -> Option<Ipv4Addr> {
    pools.iter().find_map(|p| match p {
        ipnet::IpNet::V4(v4) if v4.prefix_len() < 31 => Some(v4.broadcast()),
        _ => None,
    })
}

/// A node's own IPv6 address in the mesh: the last of its unique local prefix, which no workload
/// is ever given either, at which it answers the probe of an IPv6 address an edge forwards to it.
#[must_use]
pub fn mesh_address_v6(pools: &[ipnet::IpNet]) -> Option<std::net::Ipv6Addr> {
    pools.iter().find_map(|p| match p {
        ipnet::IpNet::V6(v6) if v6.prefix_len() < 127 && (v6.addr().octets()[0] & 0xfe) == 0xfc => {
            Some(v6.broadcast())
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests;
