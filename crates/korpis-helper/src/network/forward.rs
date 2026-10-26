// SPDX-License-Identifier: AGPL-3.0-or-later

//! The ports of the node forwarded to a workload (§3 of docs/07-networking.md). The host's table
//! has one map, from a node's port to the chain of the workload holding it, consulted for every
//! new connection to an address of the node; each workload's chain translates the destination
//! of its ports to its own address and the port inside it, and leaves the source alone, so the
//! workload sees the client's own address. The loopback addresses are not forwarded: a packet
//! from 127.0.0.1 cannot leave the host for a workload's link. Ports are unique on a node
//! whatever their transport, since the control plane allocates them so, which is what lets the
//! map be keyed by the port alone.

use super::nft::{TABLE, chain};
use serde_json::{Value, json};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::process::Command;

/// The map from a node's port to the chain of the workload it is forwarded to.
const MAP: &str = "forwarded";

/// A transport, as nftables names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Transport {
    /// TCP.
    Tcp,
    /// UDP.
    Udp,
}

impl Transport {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// One port of the node forwarded to a port inside the workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Forward {
    /// Over what.
    pub(crate) transport: Transport,
    /// The node's port.
    pub(crate) node_port: u16,
    /// The port inside the workload.
    pub(crate) container_port: u16,
}

/// The workload chain of the workload whose host link is `link`.
pub(crate) fn chain_of(link: &str) -> String {
    format!("fwd_{link}")
}

fn rule(chain: &str, expr: Vec<Value>) -> Value {
    let expr = Value::Array(expr);
    json!({"add": {"rule": {"family": "inet", "table": TABLE, "chain": chain, "expr": expr}}})
}

fn element(op: &str, elem: &Value) -> Value {
    json!({op: {"element": {"family": "inet", "table": TABLE, "name": MAP, "elem": [elem]}}})
}

/// The host's forwarding: the map, and the nat chains consulting it for a new connection to an
/// address of the node, arriving or made on the node itself.
pub(crate) fn base() -> Vec<Value> {
    let local = json!({"match": {"op": "==",
        "left": {"fib": {"result": "type", "flags": ["daddr"]}}, "right": "local"}});
    let vmap = json!({"vmap": {"key": {"payload": {"protocol": "th", "field": "dport"}},
        "data": format!("@{MAP}")}});
    let mut commands = vec![json!({"add": {"map": {"family": "inet", "table": TABLE,
        "name": MAP, "type": "inet_service", "map": "verdict"}}})];
    for hook in ["prerouting", "output"] {
        let name = format!("forward_{hook}");
        commands.push(chain(&name, "nat", hook, -100, "accept"));
        commands
            .push(json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": name}}}));
        commands.push(rule(&name, vec![local.clone(), vmap.clone()]));
    }
    commands
}

/// The rules translating `f` to `ipv4` and, where the workload has one, `ipv6`.
fn translate(chain: &str, f: Forward, (ipv4, ipv6): (Ipv4Addr, Option<Ipv6Addr>)) -> Vec<Value> {
    let proto = json!({"match": {"op": "==", "left": {"meta": {"key": "l4proto"}},
        "right": f.transport.name()}});
    let dport = json!({"match": {"op": "==",
        "left": {"payload": {"protocol": "th", "field": "dport"}}, "right": f.node_port}});
    let not_loopback = |protocol: &str, addr: &str, len: u8| {
        json!({"match": {"op": "!=", "left": {"payload": {"protocol": protocol, "field": "daddr"}},
            "right": {"prefix": {"addr": addr, "len": len}}}})
    };
    let mut rules = vec![rule(
        chain,
        vec![
            not_loopback("ip", "127.0.0.0", 8),
            proto.clone(),
            dport.clone(),
            json!({"dnat": {"family": "ip", "addr": ipv4.to_string(), "port": f.container_port}}),
        ],
    )];
    if let Some(v6) = ipv6 {
        rules.push(rule(chain, vec![
            not_loopback("ip6", "::1", 128),
            proto,
            dport,
            json!({"dnat": {"family": "ip6", "addr": v6.to_string(), "port": f.container_port}}),
        ]));
    }
    rules
}

/// Makes the workload with host link `link` hold exactly `forwards`, given the map's elements
/// as `listed`: its chain is rewritten, and the map loses every port that pointed at it or that
/// it now holds, then gains its ports. One port held by another workload's chain is taken over,
/// since the control plane, which allocates them, gave it to this one.
pub(crate) fn hold(
    link: &str,
    addresses: (Ipv4Addr, Option<Ipv6Addr>),
    forwards: &[Forward],
    listed: &[(u16, String)],
) -> Vec<Value> {
    let chain = chain_of(link);
    let mut commands = vec![
        json!({"add": {"chain": {"family": "inet", "table": TABLE, "name": chain}}}),
        json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": chain}}}),
    ];
    for f in forwards {
        commands.extend(translate(&chain, *f, addresses));
    }
    let wanted = |p: u16| forwards.iter().any(|f| f.node_port == p);
    for (port, _) in listed.iter().filter(|(p, target)| *target == chain || wanted(*p)) {
        commands.push(element("delete", &json!(port)));
    }
    for f in forwards {
        let goto = json!([f.node_port, {"goto": {"target": chain}}]);
        commands.push(element("add", &goto));
    }
    commands
}

/// Takes the workload with host link `link` out of the forwarding: its ports and its chain.
pub(crate) fn release(link: &str, listed: &Listed) -> Vec<Value> {
    let chain = chain_of(link);
    let mut commands: Vec<Value> = listed
        .ports
        .iter()
        .filter(|(_, target)| *target == chain)
        .map(|(port, _)| element("delete", &json!(port)))
        .collect();
    if listed.chains.contains(&chain) {
        commands
            .push(json!({"delete": {"chain": {"family": "inet", "table": TABLE, "name": chain}}}));
    }
    commands
}

/// What the host table holds.
#[derive(Debug, Default)]
pub(crate) struct Listed {
    /// Its forwarded ports, each with the chain it points at.
    pub(crate) ports: Vec<(u16, String)>,
    /// Its chains.
    pub(crate) chains: Vec<String>,
    /// The links its egress map names.
    pub(crate) outward: Vec<String>,
    /// The links its map of what reaches a workload from outside names.
    pub(crate) inward: Vec<String>,
    /// Its accounting: the chains' maps, the classes, and the counters.
    pub(crate) account: super::account::Listed,
}

/// What the host table holds; nothing where it is not there yet.
pub(crate) fn listed() -> io::Result<Listed> {
    let out = Command::new("nft").args(["-j", "list", "table", "inet", TABLE]).output()?;
    if !out.status.success() {
        return Ok(Listed::default());
    }
    let listing: Value = serde_json::from_slice(&out.stdout).map_err(io::Error::other)?;
    let items = listing.get("nftables").and_then(Value::as_array).cloned().unwrap_or_default();
    let chains = items
        .iter()
        .filter_map(|i| i.pointer("/chain/name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    let ports = items
        .iter()
        .filter(|i| i.pointer("/map/name").and_then(Value::as_str) == Some(MAP))
        .filter_map(|i| i.pointer("/map/elem").and_then(Value::as_array))
        .flatten()
        .filter_map(|e| {
            let port = u16::try_from(e.get(0)?.as_u64()?).ok()?;
            let target = e.get(1)?.pointer("/goto/target")?.as_str()?;
            Some((port, target.to_owned()))
        })
        .collect();
    let links = |map: &str| -> Vec<String> {
        items
            .iter()
            .filter(|i| i.pointer("/map/name").and_then(Value::as_str) == Some(map))
            .filter_map(|i| i.pointer("/map/elem").and_then(Value::as_array))
            .flatten()
            .filter_map(|e| e.get(0)?.as_str().map(str::to_owned))
            .collect()
    };
    let (outward, inward) = (links(super::egress::OUTWARD), links(super::inward::INWARD));
    let account = super::account::listed(&items);
    Ok(Listed { ports, chains, outward, inward, account })
}
