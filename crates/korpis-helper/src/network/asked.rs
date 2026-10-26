// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a request for a workload's network asks for, checked before anything is made: its
//! addresses within their pools, its ports, the destinations it opens, and its limits.

use super::forward::{Forward, Transport};
use super::{bandwidth, egress, policy};
use crate::error::{HelperError, HelperResult, identifier};
use korpis_proto::korpis::helper::v1 as helper;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A network prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Prefix {
    pub(super) addr: IpAddr,
    pub(super) len: u8,
}

pub(super) fn prefix(what: &str, text: &str) -> HelperResult<Prefix> {
    parsed(what, text, 1)
}

/// As [`prefix`], a length of zero allowed: every address of its family, which a rule may name
/// (§6 of docs/07-networking.md) and a pool may not be.
pub(super) fn range(what: &str, text: &str) -> HelperResult<Prefix> {
    parsed(what, text, 0)
}

fn parsed(what: &str, text: &str, shortest: u8) -> HelperResult<Prefix> {
    let bad = || HelperError::Refused(format!("{what} is not a prefix"));
    let (addr, len) = text.split_once('/').ok_or_else(bad)?;
    let addr: IpAddr = addr.parse().map_err(|_| bad())?;
    let len: u8 = len.parse().map_err(|_| bad())?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    if len > max || len < shortest {
        return Err(bad());
    }
    Ok(Prefix { addr, len })
}

pub(super) fn bits(ip: IpAddr) -> (u128, u8) {
    match ip {
        IpAddr::V4(v4) => (u128::from(u32::from(v4)), 32),
        IpAddr::V6(v6) => (u128::from(v6), 128),
    }
}

/// Whether `ip` is in `pool` and is neither its first address nor its last.
pub(super) fn within(ip: IpAddr, pool: Prefix) -> bool {
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
pub(super) struct Asked {
    pub(super) workload: String,
    pub(super) ipv4: Ipv4Addr,
    pub(super) ipv6: Option<Ipv6Addr>,
    pub(super) ids: (u32, u32),
    pub(super) inbound: Vec<policy::Inbound>,
    pub(super) forwards: Vec<Forward>,
    pub(super) opened: Vec<egress::Opened>,
    pub(super) rules: Vec<policy::Rule>,
    pub(super) limits: policy::Limits,
    pub(super) shaping: bandwidth::Shaping,
    pub(super) tap: bool,
    pub(super) dedicated: Option<super::dedicated::Dedicated>,
}

pub(super) fn transport(t: buffa::EnumValue<helper::Transport>) -> HelperResult<Transport> {
    match t.as_known() {
        Some(helper::Transport::TRANSPORT_TCP) => Ok(Transport::Tcp),
        Some(helper::Transport::TRANSPORT_UDP) => Ok(Transport::Udp),
        _ => Err(HelperError::Refused("a port's transport is tcp or udp".into())),
    }
}

pub(super) fn port(what: &str, p: u32, lowest: u16) -> HelperResult<u16> {
    u16::try_from(p)
        .ok()
        .filter(|p| *p >= lowest)
        .ok_or_else(|| HelperError::Refused(format!("{what} {p} is not a port it may use")))
}

/// The ports a workload accepts connections on, and those of the node forwarded to it.
pub(super) type Ports = (Vec<policy::Inbound>, Vec<Forward>);

/// The ports a request accepts and forwards, checked: a node's port is never below 1024, which
/// the node's own services hold, and none is forwarded twice.
pub(super) fn ports(r: &helper::EnsureNetworkRequest) -> HelperResult<Ports> {
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

/// The dedicated address `r` asks the workload to hold, checked: an IPv4 address a host may
/// hold, outside the node's pool of workload addresses, and an interface's name, where it names
/// one.
fn dedicated(
    r: &helper::EnsureNetworkRequest,
    pool: Prefix,
) -> HelperResult<Option<super::dedicated::Dedicated>> {
    let Some(d) = r.dedicated.as_option() else { return Ok(None) };
    let refuse = |m: &str| HelperError::Refused(format!("the dedicated address {m}"));
    let address: Ipv4Addr = d.address.parse().map_err(|_| refuse("is not an IPv4 address"))?;
    let unheld = address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || address.is_broadcast()
        || address.is_link_local();
    let (a, p) = (bits(IpAddr::V4(address)).0, bits(pool.addr).0);
    let mask = u128::from(u32::MAX.checked_shl(32 - u32::from(pool.len)).unwrap_or(0));
    if unheld || a & mask == p & mask {
        return Err(refuse("is not one a host holds for a workload"));
    }
    let name = d.interface.as_str();
    let named = name.len() < 16
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && name != "."
        && name != "..";
    if !named {
        return Err(refuse("names no interface"));
    }
    let interface = (!name.is_empty()).then(|| name.to_owned());
    Ok(Some(super::dedicated::Dedicated { address, interface }))
}

pub(super) fn asked(r: &helper::EnsureNetworkRequest) -> HelperResult<Asked> {
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
            let (why, through) = match d.reason.as_known() {
                Some(helper::DestinationReason::DESTINATION_REASON_DEPENDENCY) => {
                    ("dependency", None)
                }
                Some(helper::DestinationReason::DESTINATION_REASON_DEPENDENCY_OVERLAY) => {
                    ("dependency", Some(crate::overlay::INTERFACE))
                }
                _ => return Err(refuse("an opened destination says why it is opened")),
            };
            let (port, transport) = (port("an opened port", d.port, 1)?, transport(d.transport)?);
            Ok(egress::Opened { addr, port, transport, why, through })
        })
        .collect::<HelperResult<Vec<_>>>()?;
    let ids = (r.uid_base, r.uid_count);
    let (rules, limits) = (policy::rules(&r.egress)?, policy::limits(r)?);
    let shaping = bandwidth::asked(r.bandwidth.as_option())?;
    let (workload, tap, dedicated) = (r.workload.clone(), r.tap, dedicated(r, ipv4_pool)?);
    Ok(Asked {
        workload,
        ipv4,
        ipv6,
        ids,
        inbound,
        forwards,
        opened,
        rules,
        limits,
        shaping,
        tap,
        dedicated,
    })
}
