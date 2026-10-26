// SPDX-License-Identifier: AGPL-3.0-or-later

//! The connections a member forgets when its part in the groups changes (§3.1 of
//! docs/07-networking.md): every connection to an address it no longer holds, and every one to a
//! translated port that went elsewhere than where the port goes now, or that is refused now. A
//! UDP flow a client keeps alive with every datagram would otherwise stay translated to a node
//! its workload left. The table is read only when what the member translates or holds changed.

use super::{HELD, PREROUTING, Table};
use crate::network::conntrack::{Connection, Conntrack};
use crate::network::nft::TABLE;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::IpAddr;
use std::process::Command;

/// A translated port, as conntrack keys it: the address, the port, and the IP protocol.
type Port = (IpAddr, u16, u8);

/// What the member translated and held before its table was rewritten.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Before {
    held: BTreeSet<IpAddr>,
    translated: BTreeMap<Port, (IpAddr, u16)>,
}

fn protocol(transport: &str) -> u8 {
    if transport == "udp" { 17 } else { 6 }
}

/// The host table's listing of `what`, or nothing where it is not there yet.
fn listing(what: &[&str]) -> io::Result<Vec<Value>> {
    let out = Command::new("nft").args(["-j", "list"]).args(what).output()?;
    if !out.status.success() {
        return Ok(Vec::new());
    }
    let listing: Value = serde_json::from_slice(&out.stdout).map_err(io::Error::other)?;
    Ok(listing.get("nftables").and_then(Value::as_array).cloned().unwrap_or_default())
}

/// The match on `field` among `expr`: what its left side names it by, a payload's protocol or
/// `meta`, and its right side.
fn matched<'a>(expr: &'a [Value], field: &str) -> Option<(&'a str, &'a Value)> {
    expr.iter().find_map(|e| {
        let m = e.get("match")?;
        let left = m.get("left")?;
        let (by, named) = match (left.get("payload"), left.get("meta")) {
            (Some(p), _) => (p.get("protocol")?.as_str()?, p.get("field")?),
            (None, Some(meta)) => ("meta", meta.get("key")?),
            (None, None) => return None,
        };
        (named.as_str()? == field).then(|| Some((by, m.get("right")?))).flatten()
    })
}

/// The translation one rule of the member's chain makes. The listing folds the rule's match on
/// the transport into the match on the port where it can, naming the port `udp dport`.
fn translation(rule: &Value) -> Option<(Port, (IpAddr, u16))> {
    let expr = rule.pointer("/rule/expr")?.as_array()?;
    let address = matched(expr, "daddr")?.1.as_str()?.parse().ok()?;
    let (by, port) = matched(expr, "dport")?;
    let transport = match by {
        "th" => matched(expr, "l4proto")?.1.as_str()?,
        named => named,
    };
    let port = u16::try_from(port.as_u64()?).ok()?;
    let dnat = expr.iter().find_map(|e| e.get("dnat"))?;
    let to = dnat.get("addr")?.as_str()?.parse().ok()?;
    let to_port = u16::try_from(dnat.get("port")?.as_u64()?).ok()?;
    Some(((address, port, protocol(transport)), (to, to_port)))
}

/// What the member translates and holds as its host table says now.
///
/// # Errors
/// `nft` could not be run, or its listing was not JSON.
pub(crate) fn listed() -> io::Result<Before> {
    let mut held = BTreeSet::new();
    for set in HELD {
        held.extend(
            listing(&["set", "inet", TABLE, set])?
                .iter()
                .filter_map(|i| i.pointer("/set/elem").and_then(Value::as_array).cloned())
                .flatten()
                .filter_map(|e| e.as_str()?.parse::<IpAddr>().ok()),
        );
    }
    let translated =
        listing(&["chain", "inet", TABLE, PREROUTING])?.iter().filter_map(translation).collect();
    Ok(Before { held, translated })
}

/// What `t` translates and holds.
fn after(t: &Table) -> Before {
    let held = t.holds.iter().map(|h| h.address).collect();
    let translated = t
        .translations
        .iter()
        .filter_map(|tr| Some(((tr.address, tr.port, protocol(tr.transport)), tr.to?)))
        .collect();
    Before { held, translated }
}

/// Whether `c` is to an address no longer held, or to a port translated before whose target is
/// not where it went now.
fn stale(c: &Connection, was: &Before, now: &Before) -> bool {
    let (address, port) = c.original.destination;
    if was.held.contains(&address) && !now.held.contains(&address) {
        return true;
    }
    let key = (address, port, c.original.protocol);
    was.translated.contains_key(&key) && now.translated.get(&key) != Some(&c.reply.source)
}

/// Forgets every connection `t` leaves stale, `was` being what the member translated and held
/// before; how many it forgot.
///
/// # Errors
/// The kernel's connection tracking could not be read, or refused to forget one.
pub(crate) fn sweep(t: &Table, was: &Before) -> io::Result<usize> {
    let now = after(t);
    if *was == now {
        return Ok(0);
    }
    let mut ct = Conntrack::open()?;
    let mut forgotten = 0;
    for c in ct.connections()?.iter().filter(|c| stale(c, was, &now)) {
        ct.forget(c)?;
        forgotten += 1;
    }
    Ok(forgotten)
}
