// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's part in edge groups (§3.1 of docs/07-networking.md). As a member it translates
//! each address, transport, and port of its group's table to its workload, IPv4 and IPv6 alike,
//! marking what goes to another node so policy routing sends it into that node's GRE link, an
//! IPv4 link whichever family it carries, and refuses every other packet to an address it holds
//! but an IPv6 one's neighbour discovery. As a node behind edges it accepts GRE from each edge's
//! mesh address alone, takes from each edge's link only what that edge may reach, marks each
//! such connection with its link, and routes the replies back into it by the mark, so the
//! workload sees its client's own address and the edge reverses its translation.

mod flows;
mod links;
mod nft;

pub(crate) use flows::{listed, sweep};
pub(crate) use links::{Kept, keep};
pub(crate) use nft::rules;

use crate::error::{HelperError, HelperResult};
use korpis_proto::korpis::helper::v1 as helper;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// The mark of what a member sends into a GRE link, and the table routing it there.
pub(crate) const EDGE_MARK: u32 = 0x4b45_0000;
/// The marks of what arrives from an edge's link, each that link's index beside it.
pub(crate) const SOURCE_MARK: u32 = 0x4b46_0000;
/// The table routing the node's own mesh addresses where the main table has no route.
pub(crate) const MESH_TABLE: u32 = 0x4b47_0000;
/// What tells Korpis's marks from any other's.
const KORPIS_MARKS: u32 = 0x4b00_0000;
const MARKS_MASK: u32 = 0xff00_0000;
/// The links' names begin so.
pub(crate) const LINKS: &str = "kg";
/// A GRE link inside the mesh: the mesh's MTU less GRE's 24 bytes over IPv4.
pub(crate) const MTU: u32 = super::MTU - 24;

/// The sets and maps the rules name, those of addresses one of each family.
const SOURCES: &str = "edge_sources";
const ADMITTED: [&str; 2] = ["edge_admitted", "edge_admitted6"];
const SOURCE_MARKS: &str = "edge_marks";
const HELD: [&str; 2] = ["edge_held", "edge_held6"];
/// The member's chain of translations.
const PREROUTING: &str = "edge_prerouting";
/// What the helper put each held address on, so it removes only what it put there.
pub(crate) const HELD_ON: [&str; 2] = ["edge_held_on", "edge_held_on6"];
/// The base chains of the member's side.
const CHAINS: [(&str, &str, &str, i32); 4] = [
    (PREROUTING, "nat", "prerouting", -105),
    ("edge_marks_in", "filter", "prerouting", -150),
    ("edge_marks_out", "route", "output", -150),
    ("edge_input", "filter", "input", 0),
];

/// One address, transport, and port a member translates, to an address of its own family; the
/// GRE link it is carried in is IPv4 whatever it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Translation {
    pub(crate) address: IpAddr,
    pub(crate) port: u16,
    pub(crate) transport: &'static str,
    pub(crate) via: Option<Ipv4Addr>,
    pub(crate) to: Option<(IpAddr, u16)>,
}

/// One address a member holds, and the interface it is put on; the loopback where none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hold {
    pub(crate) address: IpAddr,
    pub(crate) interface: String,
}

/// What one edge may reach of this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Admit {
    pub(crate) source: Ipv4Addr,
    pub(crate) destination: IpAddr,
    pub(crate) transport: &'static str,
    pub(crate) port: u16,
}

/// The node's part in edge groups, as asked and checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Table {
    pub(crate) mesh: Option<Ipv4Addr>,
    pub(crate) mesh6: Option<Ipv6Addr>,
    pub(crate) tunnels: Vec<Ipv4Addr>,
    pub(crate) translations: Vec<Translation>,
    pub(crate) holds: Vec<Hold>,
    pub(crate) admits: Vec<Admit>,
}

/// The name of the link to the node whose mesh address is `remote`.
pub(crate) fn link_name(remote: Ipv4Addr) -> String {
    format!("{LINKS}{:08x}", u32::from(remote))
}

fn transport(t: helper::Transport, what: &str) -> HelperResult<&'static str> {
    match t {
        helper::Transport::TRANSPORT_TCP => Ok("tcp"),
        helper::Transport::TRANSPORT_UDP => Ok("udp"),
        helper::Transport::TRANSPORT_UNSPECIFIED => {
            Err(HelperError::Refused(format!("{what}: no transport")))
        }
    }
}

/// `text` read as an address of the kind `A` is, IPv4, IPv6, or either.
fn parsed<A: std::str::FromStr>(text: &str, what: &str) -> HelperResult<A> {
    text.parse().map_err(|_| HelperError::Refused(format!("{what}: {text:?} is no such address")))
}

fn address(text: &str, what: &str) -> HelperResult<Ipv4Addr> {
    parsed(text, what)
}

fn optional<A: std::str::FromStr>(text: &str, what: &str) -> HelperResult<Option<A>> {
    if text.is_empty() { Ok(None) } else { parsed(text, what).map(Some) }
}

fn port(n: u32, what: &str) -> HelperResult<u16> {
    u16::try_from(n)
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| HelperError::Refused(format!("{what}: {n} is no port")))
}

fn translation(t: &helper::EdgeTranslation) -> HelperResult<Translation> {
    let what = "an edge translation";
    let known = |e: buffa::EnumValue<helper::Transport>| {
        e.as_known().unwrap_or(helper::Transport::TRANSPORT_UNSPECIFIED)
    };
    let address: IpAddr = parsed(&t.address, what)?;
    let to = match optional::<IpAddr>(&t.to_address, what)? {
        Some(a) if a.is_ipv4() != address.is_ipv4() => {
            return Err(HelperError::Refused(format!("{what}: {a} is not of {address}'s family")));
        }
        Some(a) => Some((a, port(t.to_port, what)?)),
        None => None,
    };
    Ok(Translation {
        address,
        port: port(t.port, what)?,
        transport: transport(known(t.transport), what)?,
        via: optional(&t.via, what)?,
        to,
    })
}

/// What `r` asks of the node's part in edge groups, checked.
///
/// # Errors
/// The first thing it names wrongly, and why.
pub(crate) fn table(r: &helper::KeepOverlayRequest) -> HelperResult<Table> {
    let mesh = optional(&r.mesh_address, "the mesh address")?;
    let mesh6 = optional(&r.mesh_address_v6, "the mesh's IPv6 address")?;
    let Some(e) = r.edges.as_option() else {
        return Ok(Table { mesh, mesh6, ..Table::default() });
    };
    let tunnels = e.tunnels.iter().map(|t| address(t, "an edge tunnel"));
    let holds = e.holds.iter().map(|h| {
        let interface = if h.interface.is_empty() { "lo".to_owned() } else { h.interface.clone() };
        Ok(Hold { address: parsed(&h.address, "an edge hold")?, interface })
    });
    let admits = e.admits.iter().map(|a| {
        let what = "an edge admission";
        let known = a.transport.as_known().unwrap_or(helper::Transport::TRANSPORT_UNSPECIFIED);
        Ok(Admit {
            source: address(&a.source, what)?,
            destination: parsed(&a.destination, what)?,
            transport: transport(known, what)?,
            port: port(a.port, what)?,
        })
    });
    let table = Table {
        mesh,
        mesh6,
        tunnels: tunnels.collect::<HelperResult<_>>()?,
        translations: e.translations.iter().map(translation).collect::<HelperResult<_>>()?,
        holds: holds.collect::<HelperResult<_>>()?,
        admits: admits.collect::<HelperResult<_>>()?,
    };
    if table.mesh.is_none() && !table.tunnels.is_empty() {
        return Err(HelperError::Refused("edge tunnels need a mesh address".into()));
    }
    Ok(table)
}

#[cfg(test)]
mod tests;
