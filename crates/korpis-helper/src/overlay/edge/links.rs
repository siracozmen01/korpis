// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the node's part in edge groups keeps outside nftables: its mesh address on the mesh's
//! interface, a GRE link to each node it tunnels with and no other, the rules and routes that
//! send a member's translated packets into the right link and a node's replies back into the
//! link they came by, and the addresses a member holds on their interfaces.

use super::{EDGE_MARK, HELD_ON, LINKS, MTU, SOURCE_MARK, Table, link_name};
use crate::network::nft::TABLE;
use crate::network::rtnl::{MarkRule, Rtnl};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::process::Command;

const EXACT: u32 = 0xffff_ffff;

/// A table's routes: each destination with its prefix, and the link it leaves by.
type Routes = Vec<((IpAddr, u8), u32)>;
/// Each rule asked for by its mark, with its table's routes.
type Wanted = BTreeMap<u32, (MarkRule, Routes)>;

/// The kernel's check of a packet's source, on link `name`, made by its mark: what arrives through
/// the link has a client's source, which the main table routes elsewhere or nowhere, and the
/// kernel refuses it in strict mode and, the link having no IPv4 address, in loose mode too. With
/// the link's mark on each packet (the host table puts it there), the check finds its route back
/// into the link and passes whatever the host's own setting, which the helper leaves as it is. A
/// setting of that link alone.
fn marked_sources(name: &str) -> io::Result<()> {
    std::fs::write(format!("/proc/sys/net/ipv4/conf/{name}/src_valid_mark"), "1")
}

/// Link `name`'s rules for each name in `wanted`, and no other link's.
fn keep_link_rules(rtnl: &mut Rtnl, wanted: &[&String]) -> io::Result<()> {
    for name in rtnl.link_rules()? {
        if !wanted.contains(&&name) {
            rtnl.delete_link_rules(&name)?;
        }
    }
    for name in wanted {
        rtnl.add_link_rules(name)?;
    }
    Ok(())
}

/// The GRE links `t` asks for, made, sized, checked by their marks, and up, and every other
/// removed; each by the mesh address at its other end, with its name and index.
fn tunnels(rtnl: &mut Rtnl, t: &Table) -> io::Result<BTreeMap<Ipv4Addr, (String, u32)>> {
    let wanted: BTreeMap<String, Ipv4Addr> =
        t.tunnels.iter().map(|r| (link_name(*r), *r)).collect();
    for (index, name) in rtnl.links_named(LINKS)? {
        if !wanted.contains_key(&name) {
            rtnl.delete_link(index)?;
        }
    }
    let mut out = BTreeMap::new();
    let Some(mesh) = t.mesh else { return Ok(out) };
    for (name, remote) in wanted {
        rtnl.add_gre(&name, (mesh, remote))?;
        let index = rtnl
            .link_index(&name)?
            .ok_or_else(|| io::Error::other(format!("the edge link {name} is gone")))?;
        rtnl.set_mtu(index, MTU)?;
        marked_sources(&name)?;
        rtnl.set_up(index)?;
        out.insert(remote, (name, index));
    }
    Ok(out)
}

/// Each rule `t` asks for and the routes of its table: a member's into the link of each node it
/// carries a workload's address to, and a node's default route back into each edge's link.
fn routing(t: &Table, links: &BTreeMap<Ipv4Addr, (String, u32)>) -> Wanted {
    let mut out = BTreeMap::new();
    let carried: Routes = t
        .translations
        .iter()
        .filter_map(|tr| {
            let (to, _) = tr.to?;
            let (_, index) = links.get(&tr.via?)?;
            Some(((IpAddr::V4(to), 32), *index))
        })
        .collect();
    if !carried.is_empty() {
        let rule = MarkRule { mark: EDGE_MARK, mask: EXACT, table: EDGE_MARK };
        out.insert(EDGE_MARK, (rule, carried));
    }
    for a in &t.admits {
        if let Some((_, index)) = links.get(&a.source) {
            let mark = SOURCE_MARK | (index & 0xffff);
            let back = vec![((IpAddr::from([0, 0, 0, 0]), 0), *index)];
            out.insert(mark, (MarkRule { mark, mask: EXACT, table: mark }, back));
        }
    }
    out
}

/// Keeps the rules and routes `wanted` names, and removes every other rule of Korpis's marks
/// with its table's routes.
fn keep_routing(rtnl: &mut Rtnl, wanted: &Wanted) -> io::Result<()> {
    let ours = |m: u32| m & 0xffff_0000 == EDGE_MARK || m & 0xffff_0000 == SOURCE_MARK;
    for r in rtnl.mark_rules()?.into_iter().filter(|r| ours(r.mark)) {
        if wanted.get(&r.mark).is_none_or(|(w, _)| *w != r) {
            rtnl.delete_mark_rule(r)?;
            for (dest, index) in rtnl.table_routes(r.table)? {
                rtnl.delete_table_route(r.table, dest, index)?;
            }
        }
    }
    for (rule, routes) in wanted.values() {
        for (dest, index) in rtnl.table_routes(rule.table)? {
            if !routes.contains(&(dest, index)) {
                rtnl.delete_table_route(rule.table, dest, index)?;
            }
        }
        for (dest, index) in routes {
            rtnl.put_table_route(rule.table, *dest, *index)?;
        }
        rtnl.add_mark_rule(*rule)?;
    }
    Ok(())
}

/// The addresses the helper put on interfaces for a member's holds, as the host table lists them.
fn held_before() -> io::Result<Vec<(Ipv4Addr, String)>> {
    let out = Command::new("nft").args(["-j", "list", "set", "inet", TABLE, HELD_ON]).output()?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    let listing: Value = serde_json::from_slice(&out.stdout).map_err(io::Error::other)?;
    let items = listing.get("nftables").and_then(Value::as_array).cloned().unwrap_or_default();
    let elems = items.iter().filter_map(|i| i.pointer("/set/elem").and_then(Value::as_array));
    Ok(elems
        .flatten()
        .filter_map(|e| {
            let pair = e.pointer("/concat")?.as_array()?;
            let address = pair.first()?.as_str()?.parse().ok()?;
            Some((address, pair.get(1)?.as_str()?.to_owned()))
        })
        .collect())
}

/// Puts each address `t` holds on its interface, and takes off each the helper put on one
/// before that `t` no longer holds there.
fn keep_holds(rtnl: &mut Rtnl, t: &Table) -> io::Result<()> {
    let wanted: Vec<(Ipv4Addr, String)> =
        t.holds.iter().map(|h| (h.address, h.interface.clone())).collect();
    for (address, interface) in held_before()? {
        if !wanted.contains(&(address, interface.clone()))
            && let Some(index) = rtnl.link_index(&interface)?
        {
            rtnl.delete_host_address(index, address)?;
        }
    }
    for (address, interface) in &wanted {
        let index = rtnl
            .link_index(interface)?
            .ok_or_else(|| io::Error::other(format!("no interface {interface:?} to hold on")))?;
        rtnl.add_host_address(index, *address)?;
    }
    Ok(())
}

/// Keeps everything `t` asks for outside nftables, the mesh's interface being link
/// `overlay`; each GRE link by the mesh address at its other end, with its name and index.
///
/// # Errors
/// Netlink refusing a change, or an interface to hold an address on missing.
pub(crate) fn keep(
    rtnl: &mut Rtnl,
    overlay: u32,
    t: &Table,
) -> io::Result<BTreeMap<Ipv4Addr, (String, u32)>> {
    if let Some(mesh) = t.mesh {
        rtnl.add_host_address(overlay, mesh)?;
    }
    let links = tunnels(rtnl, t)?;
    keep_link_rules(rtnl, &links.values().map(|(name, _)| name).collect::<Vec<_>>())?;
    keep_routing(rtnl, &routing(t, &links))?;
    keep_holds(rtnl, t)?;
    Ok(links)
}
