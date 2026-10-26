// SPDX-License-Identifier: AGPL-3.0-or-later

//! What reaches a workload from outside the node (§4 of docs/07-networking.md): a new connection
//! forwarded to a workload's link from neither another workload's link nor the mesh is let in
//! only where the node translated it, which is a published port of the node's, or to a port of
//! an endpoint whose exposure is reached at the workload's own address. Everything else from
//! outside is dropped and counted under `exposure`, so an `overlay_only` endpoint, or an
//! `ingress` one, is not reached past its node by whoever can route to the workload's address,
//! which a routed IPv6 prefix makes the whole internet. Each workload has a chain of its own,
//! reached through a map keyed by its link, holding its ports reached from outside. What a
//! workload's link sends, and what arrives through the mesh, is judged elsewhere (§6, §9.1).

use super::nft::TABLE;
use super::policy::Inbound;
use serde_json::{Value, json};

/// The map from a workload's link to its chain of ports reached from outside.
pub(crate) const INWARD: &str = "inward";
const LINKS: &str = "kw*";

fn rule(chain: &str, comment: Option<&str>, expr: Vec<Value>) -> Value {
    let mut r =
        json!({"family": "inet", "table": TABLE, "chain": chain, "expr": Value::Array(expr)});
    if let (Some(c), Some(o)) = (comment, r.as_object_mut()) {
        o.insert("comment".into(), json!(c));
    }
    json!({"add": {"rule": r}})
}

fn when(op: &str, key: &str, right: &str) -> Value {
    json!({"match": {"op": op, "left": {"meta": {"key": key}}, "right": right}})
}

/// A new connection to a workload's link from outside the node: from neither a workload's link
/// nor the mesh.
fn from_outside() -> Vec<Value> {
    vec![
        when("==", "oifname", LINKS),
        when("!=", "iifname", LINKS),
        when("!=", "iifname", crate::overlay::INTERFACE),
        json!({"match": {"op": "in", "left": {"ct": {"key": "state"}}, "right": ["new"]}}),
    ]
}

/// The map, and the rows of the host's forward chain `c` for what comes from outside: what the
/// node translated, then each workload's own chain, then the rest dropped and counted.
pub(crate) fn base(c: &str) -> Vec<Value> {
    let translated = json!({"match": {"op": "in", "left": {"ct": {"key": "status"}},
        "right": "dnat"}});
    let vmap = json!({"vmap": {"key": {"meta": {"key": "oifname"}}, "data": format!("@{INWARD}")}});
    vec![
        json!({"add": {"map": {"family": "inet", "table": TABLE, "name": INWARD,
            "type": "ifname", "map": "verdict"}}}),
        rule(c, None, [from_outside(), vec![translated, json!({"accept": null})]].concat()),
        rule(c, None, [from_outside(), vec![vmap]].concat()),
        rule(c, Some("exposure"), [from_outside(), drop_counted()].concat()),
    ]
}

fn drop_counted() -> Vec<Value> {
    vec![json!({"counter": null}), json!({"drop": null})]
}

/// The chain of the workload whose host link is `link`.
pub(crate) fn chain_of(link: &str) -> String {
    format!("in_{link}")
}

/// Makes the chain of the workload with host link `link`: each of `inbound` reached from
/// outside, accepted; and its entry in the map, given whether it is `listed`.
pub(crate) fn hold(link: &str, inbound: &[Inbound], listed: bool) -> Vec<Value> {
    let c = chain_of(link);
    let mut commands = vec![
        json!({"add": {"chain": {"family": "inet", "table": TABLE, "name": c}}}),
        json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": c}}}),
    ];
    for i in inbound.iter().filter(|i| i.outside) {
        let expr = vec![
            json!({"match": {"op": "==", "left": {"meta": {"key": "l4proto"}},
                "right": i.transport.name()}}),
            json!({"match": {"op": "==", "left": {"payload": {"protocol": "th", "field": "dport"}},
                "right": i.port}}),
            json!({"accept": null}),
        ];
        commands.push(rule(&c, None, expr));
    }
    if !listed {
        let elem = json!([link, {"jump": {"target": c}}]);
        commands.push(json!({"add": {"element": {"family": "inet", "table": TABLE,
            "name": INWARD, "elem": [elem]}}}));
    }
    commands
}

/// Takes the workload with host link `link` out: its map entry, if `listed`, and its chain, if
/// `chained`.
pub(crate) fn release(link: &str, (listed, chained): (bool, bool)) -> Vec<Value> {
    let mut commands = Vec::new();
    if listed {
        commands.push(json!({"delete": {"element": {"family": "inet", "table": TABLE,
            "name": INWARD, "elem": [link]}}}));
    }
    if chained {
        commands.push(json!({"delete": {"chain": {"family": "inet", "table": TABLE,
            "name": chain_of(link)}}}));
    }
    commands
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use crate::network::forward::Transport;

    fn port(port: u16, outside: bool) -> Inbound {
        Inbound {
            transport: Transport::Tcp,
            port,
            rules: Vec::new(),
            max_connections: 0,
            new_per_sec: 0,
            shaping: (0, 0),
            accounted: false,
            class: 0,
            outside,
        }
    }

    #[test]
    fn only_ports_reached_from_outside_are_accepted_and_the_rest_from_outside_is_dropped() {
        let held = Value::Array(hold("kwabc", &[port(8080, true), port(5432, false)], false));
        let held = held.to_string();
        assert!(held.contains("8080") && !held.contains("5432"), "{held}");
        assert!(held.contains(r#""element""#), "listed once made");
        assert!(!Value::Array(hold("kwabc", &[], true)).to_string().contains(r#""element""#));
        let rows = Value::Array(base("workload_forward")).to_string();
        assert!(rows.contains("exposure") && rows.contains("dnat") && rows.contains(INWARD));
        assert!(rows.contains(crate::overlay::INTERFACE), "the mesh is judged by its own chains");
    }
}
