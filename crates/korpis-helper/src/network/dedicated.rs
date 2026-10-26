// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's dedicated address (§3.7 of docs/07-networking.md). Every new connection to it is
//! translated to the workload's own IPv4 address, all ports alike, in chains ahead of every other
//! translation on the node, so no port the node publishes for anyone else is reached at it; and
//! what the workload sends beyond the node, by any interface but another workload's link and the
//! mesh's, leaves translated to it, ahead of the masquerade. A map from the address to the
//! workload's chain holds the first, and a map from the workload's link to the address the
//! second, so taking a workload out needs only its link. An address of a pool on the node's link
//! is added to the pool's interface as a host address, and the workload's chain says so in its
//! rule's comment, so only an address the helper added is ever removed.

use super::nft::{TABLE, chain};
use super::rtnl::Rtnl;
use crate::error::{HelperError, HelperResult};
use serde_json::{Value, json};
use std::net::Ipv4Addr;

/// The map from a dedicated address to the chain of the workload holding it.
pub(crate) const INWARD: &str = "dedicated_in";
/// The map from a workload's link to the dedicated address it sends from.
pub(crate) const OUTWARD: &str = "dedicated_out";
/// The base chains translating what arrives, made on the host itself or not.
pub(crate) const CHAINS: [&str; 2] = ["dedicated_prerouting", "dedicated_output"];
/// Ahead of the node's forwarded ports and of any other table's destination translation.
const PRIORITY: i32 = -110;
const LINKS: &str = "kw*";

/// A dedicated address, as asked: the address, and the interface it is answered on, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dedicated {
    pub(crate) address: Ipv4Addr,
    pub(crate) interface: Option<String>,
}

/// What the host table holds of dedicated addresses.
#[derive(Debug, Default)]
pub(crate) struct Listed {
    /// Each address, with the chain it points at.
    pub(crate) inward: Vec<(Ipv4Addr, String)>,
    /// The links the outward map names.
    pub(crate) outward: Vec<String>,
    /// Each workload chain whose address the helper added to an interface, with the interface.
    pub(crate) added: Vec<(String, String)>,
}

/// The workload chain of the workload whose host link is `link`.
pub(crate) fn chain_of(link: &str) -> String {
    format!("ded_{link}")
}

fn element(map: &str, op: &str, elem: &Value) -> Value {
    json!({op: {"element": {"family": "inet", "table": TABLE, "name": map, "elem": [elem]}}})
}

fn meta(op: &str, key: &str, right: &str) -> Value {
    json!({"match": {"op": op, "left": {"meta": {"key": key}}, "right": right}})
}

fn rule(chain: &str, expr: Vec<Value>, comment: Option<&str>) -> Value {
    let mut r =
        json!({"family": "inet", "table": TABLE, "chain": chain, "expr": Value::Array(expr)});
    if let (Some(c), Some(o)) = (comment, r.as_object_mut()) {
        o.insert("comment".into(), json!(c));
    }
    json!({"add": {"rule": r}})
}

/// The maps, and the chains translating what arrives at an address in the first.
pub(crate) fn base() -> Vec<Value> {
    let mut commands = vec![
        json!({"add": {"map": {"family": "inet", "table": TABLE, "name": INWARD,
            "type": "ipv4_addr", "map": "verdict"}}}),
        json!({"add": {"map": {"family": "inet", "table": TABLE, "name": OUTWARD,
            "type": "ifname", "map": "ipv4_addr"}}}),
    ];
    let vmap = json!({"vmap": {"key": {"payload": {"protocol": "ip", "field": "daddr"}},
        "data": format!("@{INWARD}")}});
    for (name, hook) in CHAINS.into_iter().zip(["prerouting", "output"]) {
        commands.push(chain(name, "nat", hook, PRIORITY, "accept"));
        commands
            .push(json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": name}}}));
        commands.push(rule(name, vec![meta("==", "nfproto", "ipv4"), vmap.clone()], None));
    }
    commands
}

/// The row of the host's `postrouting` chain, ahead of its masquerade, translating what a
/// workload with a dedicated address sends beyond the node to that address.
pub(crate) fn outward(postrouting: &str) -> Value {
    let to = json!({"map": {"key": {"meta": {"key": "iifname"}}, "data": format!("@{OUTWARD}")}});
    rule(
        postrouting,
        vec![
            meta("==", "nfproto", "ipv4"),
            meta("==", "iifname", LINKS),
            meta("!=", "oifname", LINKS),
            meta("!=", "oifname", crate::overlay::INTERFACE),
            json!({"snat": {"family": "ip", "addr": to}}),
        ],
        None,
    )
}

/// Makes the workload with host link `link` and IPv4 address `ipv4` hold `held`, given what the
/// table holds as `listed`: its chain rewritten, saying on which interface its address was added
/// where `added` names one, and both maps losing what named it, then gaining what it holds now.
pub(crate) fn hold(
    (link, ipv4): (&str, Ipv4Addr),
    held: Option<&Dedicated>,
    added: Option<&str>,
    listed: &Listed,
) -> Vec<Value> {
    let chain = chain_of(link);
    let mut commands = release(link, listed, false);
    commands.push(json!({"add": {"chain": {"family": "inet", "table": TABLE, "name": chain}}}));
    commands.push(json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": chain}}}));
    let Some(d) = held else { return commands };
    let comment = added.map(|i| format!("on {i}"));
    let dnat = json!({"dnat": {"family": "ip", "addr": ipv4.to_string()}});
    commands.push(rule(&chain, vec![dnat], comment.as_deref()));
    let address = d.address.to_string();
    // An address another workload's chain holds is taken over: the control plane gave it here.
    if listed.inward.iter().any(|(a, target)| *a == d.address && *target != chain) {
        commands.push(element(INWARD, "delete", &json!(address)));
    }
    commands.push(element(INWARD, "add", &json!([address, {"goto": {"target": chain}}])));
    commands.push(element(OUTWARD, "add", &json!([link, address])));
    commands
}

/// Takes the workload with host link `link` out of both maps, and with `chain` its chain too.
pub(crate) fn release(link: &str, listed: &Listed, chained: bool) -> Vec<Value> {
    let chain = chain_of(link);
    let mut commands: Vec<Value> = listed
        .inward
        .iter()
        .filter(|(_, target)| *target == chain)
        .map(|(address, _)| element(INWARD, "delete", &json!(address.to_string())))
        .collect();
    if listed.outward.iter().any(|l| l == link) {
        commands.push(element(OUTWARD, "delete", &json!(link)));
    }
    if chained {
        commands
            .push(json!({"delete": {"chain": {"family": "inet", "table": TABLE, "name": chain}}}));
    }
    commands
}

/// The address the workload with host link `link` holds now, as the table says, with the
/// interface the helper added it to, if it did.
pub(crate) fn holding(link: &str, listed: &Listed) -> Option<(Ipv4Addr, Option<String>)> {
    let chain = chain_of(link);
    let (address, _) = listed.inward.iter().find(|(_, target)| *target == chain)?;
    let added = listed.added.iter().find(|(c, _)| *c == chain).map(|(_, i)| i.clone());
    Some((*address, added))
}

/// Removes `address` from `interface`, where the helper added it there.
fn unlink(nl: &mut Rtnl, address: Ipv4Addr, interface: &str) -> HelperResult<()> {
    if let Some(index) = nl.link_index(interface)? {
        nl.delete_host_address(index, address)?;
    }
    Ok(())
}

/// Makes the workload with host link `link` and address `ipv4` hold `held` on the node: the
/// address it held before, if another, taken off the interface the helper added it to, and the
/// one it holds added to its pool's interface, if it names one; then the rows saying so.
///
/// # Errors
/// The interface is not on the node, or route netlink refused.
pub(crate) fn ensure(
    (link, ipv4): (&str, Ipv4Addr),
    held: Option<&Dedicated>,
    listed: &Listed,
) -> HelperResult<Vec<Value>> {
    let mut nl = Rtnl::open()?;
    let before = holding(link, listed);
    let same = |b: &(Ipv4Addr, Option<String>)| {
        held.is_some_and(|d| d.address == b.0 && d.interface == b.1)
    };
    if let Some((address, Some(interface))) = before.as_ref().filter(|b| !same(b)) {
        unlink(&mut nl, *address, interface)?;
    }
    let mut added = before.filter(same).and_then(|(_, i)| i);
    if let Some(d) = held
        && added.is_none()
        && let Some(interface) = &d.interface
    {
        let missing = || HelperError::Failed(format!("the node has no interface {interface}"));
        let index = nl.link_index(interface)?.ok_or_else(missing)?;
        // An address the node held already is the operator's, and stays when the workload goes.
        added = nl.add_host_address(index, d.address)?.then(|| interface.clone());
    }
    Ok(hold((link, ipv4), held, added.as_deref(), listed))
}

/// Takes the workload with host link `link` out: the address it held off the interface the
/// helper added it to, and the rows naming it.
///
/// # Errors
/// Route netlink refused.
pub(crate) fn unheld(link: &str, listed: &Listed, chained: bool) -> HelperResult<Vec<Value>> {
    if let Some((address, Some(interface))) = holding(link, listed) {
        unlink(&mut Rtnl::open()?, address, &interface)?;
    }
    Ok(release(link, listed, chained))
}

/// What a listing of the host table, as `items`, holds of dedicated addresses.
pub(crate) fn listed(items: &[Value]) -> Listed {
    let elems = |map: &str| -> Vec<Value> {
        items
            .iter()
            .filter(|i| i.pointer("/map/name").and_then(Value::as_str) == Some(map))
            .filter_map(|i| i.pointer("/map/elem").and_then(Value::as_array))
            .flatten()
            .cloned()
            .collect()
    };
    let inward = elems(INWARD)
        .iter()
        .filter_map(|e| {
            let address = e.get(0)?.as_str()?.parse().ok()?;
            let target = e.get(1)?.pointer("/goto/target")?.as_str()?;
            Some((address, target.to_owned()))
        })
        .collect();
    let outward =
        elems(OUTWARD).iter().filter_map(|e| e.get(0)?.as_str().map(str::to_owned)).collect();
    let added = items
        .iter()
        .filter_map(|i| {
            let chain = i.pointer("/rule/chain")?.as_str()?;
            let on = i.pointer("/rule/comment")?.as_str()?.strip_prefix("on ")?;
            chain.starts_with("ded_").then(|| (chain.to_owned(), on.to_owned()))
        })
        .collect();
    Listed { inward, outward, added }
}

#[cfg(test)]
mod tests;
