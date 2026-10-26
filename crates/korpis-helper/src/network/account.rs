// SPDX-License-Identifier: AGPL-3.0-or-later

//! What each workload's link carries, counted in the host's table, and the class of the node's
//! uplinks its packets go in (§7 of docs/07-networking.md). Three base chains, on the forward,
//! input, and output hooks, ahead of the filter, send every packet from a workload's link to its
//! `from_` chain and every packet to it to its `to_` chain, through two maps keyed by the link;
//! neither chain decides anything but a cut-off, so the filter's verdicts are untouched. The
//! counters are named objects, which a chain rewritten keeps counting into, and which the meter
//! reads; the class is kept in a map from the link, so the helper allocates it without a ledger
//! of its own and finds it again to remove it. The `to_` chain also decides the class on the
//! host's side of the link of everything sent to the workload: an endpoint's own, by its port,
//! where it has a limit of its own, and otherwise none, so a priority set before, by the
//! sender's chain or by a program on the node, never lands in one.

use super::forward::Transport;
use super::nft::{TABLE, chain};
use super::policy::Inbound;
use korpis_proto::korpis::helper::v1 as helper;
use serde_json::{Value, json};

/// The map from a workload's link to its `from_` chain.
const FROM: &str = "acct_from";
/// The map from a workload's link to its `to_` chain.
const TO: &str = "acct_to";
/// The map from a workload's link to its class on the uplinks.
const CLASS: &str = "acct_class";
/// What every workload's link is named with, as nftables matches a name by its beginning.
const LINKS: &str = "kw*";

fn obj(kind: &str, fields: Value) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("family".into(), json!("inet"));
    o.insert("table".into(), json!(TABLE));
    if let Value::Object(f) = fields {
        o.extend(f);
    }
    let mut outer = serde_json::Map::new();
    outer.insert(kind.to_owned(), Value::Object(o));
    Value::Object(outer)
}

fn rule(chain: &str, expr: Vec<Value>) -> Value {
    json!({"add": obj("rule", json!({"chain": chain, "expr": Value::Array(expr)}))})
}

fn meta(key: &str) -> Value {
    json!({"meta": {"key": key}})
}

fn is(op: &str, left: Value, right: Value) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("op".into(), json!(op));
    m.insert("left".into(), left);
    m.insert("right".into(), right);
    json!({ "match": m })
}

fn vmap(key: &str, map: &str) -> Value {
    json!({"vmap": {"key": meta(key), "data": format!("@{map}")}})
}

/// The maps and the base chains, written with the host table's base.
pub(crate) fn base() -> Vec<Value> {
    let map = |name: &str, kind: &str| json!({"add": obj("map", json!({"name": name, "type": "ifname", "map": kind}))});
    let mut commands = vec![map(FROM, "verdict"), map(TO, "verdict"), map(CLASS, "mark")];
    let hooks = [
        ("account_forward", "forward", vec![("iifname", FROM), ("oifname", TO)]),
        ("account_input", "input", vec![("iifname", FROM)]),
        ("account_output", "output", vec![("oifname", TO)]),
    ];
    for (name, hook, keys) in hooks {
        commands.push(chain(name, "filter", hook, -150, "accept"));
        commands.push(json!({"flush": obj("chain", json!({"name": name}))}));
        for (key, map) in keys {
            commands.push(rule(name, vec![vmap(key, map)]));
        }
    }
    commands
}

/// The two chains of the workload whose host link is `link`.
pub(crate) fn chains_of(link: &str) -> [String; 2] {
    [format!("from_{link}"), format!("to_{link}")]
}

/// A counter's name: of `link`, for `what`.
pub(crate) fn counter(link: &str, what: &str) -> String {
    format!("{link}_{what}")
}

/// What a port's counters are named for, by direction: `tcp_25565_in`.
pub(crate) fn port_counter(link: &str, (transport, port): (Transport, u16), dir: &str) -> String {
    counter(link, &format!("{}_{port}_{dir}", transport.name()))
}

/// What one workload's chains hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Own<'a> {
    /// Its endpoints' ports.
    pub(crate) ports: &'a [Inbound],
    /// Its class on the uplinks, if it reserves any.
    pub(crate) class: Option<u32>,
    /// Nothing it sends leaves for beyond the node: its egress budget is spent with a cut-off.
    pub(crate) cut_off: bool,
}

/// The counters `own` keeps for `link`.
pub(crate) fn counters(link: &str, own: &Own<'_>) -> Vec<String> {
    let mut names = vec![counter(link, "egress")];
    for i in own.ports.iter().filter(|i| i.accounted) {
        names.push(port_counter(link, (i.transport, i.port), "in"));
        names.push(port_counter(link, (i.transport, i.port), "out"));
    }
    names
}

fn port_match(dir: &str, (transport, port): (Transport, u16)) -> [Value; 2] {
    let field = if dir == "in" { "dport" } else { "sport" };
    [
        is("==", meta("l4proto"), json!(transport.name())),
        is("==", json!({"payload": {"protocol": "th", "field": field}}), json!(port)),
    ]
}

fn counted(name: &str) -> Value {
    json!({"counter": name})
}

fn priority(value: &str) -> Value {
    json!({"mangle": {"key": meta("priority"), "value": value}})
}

/// The rows of `link`'s chains, rewritten whole, and its entries in the maps; `stale` are the
/// counters of the link its endpoints no longer need.
pub(crate) fn hold(link: &str, own: &Own<'_>, stale: &[String]) -> Vec<Value> {
    let [from, to] = chains_of(link);
    let mut c = Vec::new();
    for name in [&from, &to] {
        c.push(json!({"add": obj("chain", json!({"name": name}))}));
        c.push(json!({"flush": obj("chain", json!({"name": name}))}));
    }
    for name in counters(link, own) {
        c.push(json!({"add": obj("counter", json!({"name": name}))}));
    }
    for name in stale {
        c.push(json!({"delete": obj("counter", json!({"name": name}))}));
    }
    let beyond = || is("!=", meta("oifname"), json!(LINKS));
    if own.cut_off {
        let row = vec![beyond(), json!({"counter": null}), json!({"drop": null})];
        c.push(rule(&from, row));
    }
    if let Some(class) = own.class {
        c.push(rule(&from, vec![priority(&super::shape::id(class))]));
    }
    c.push(rule(&to, vec![priority("none")]));
    for i in own.ports {
        let p = (i.transport, i.port);
        if i.accounted {
            let [l4, port] = port_match("out", p);
            c.push(rule(&from, vec![l4, port, counted(&port_counter(link, p, "out"))]));
        }
        let mut row = Vec::from(port_match("in", p));
        if i.accounted {
            row.push(counted(&port_counter(link, p, "in")));
        }
        if i.shaping.1 > 0 {
            row.push(priority(&super::shape::link_id(i.class)));
        }
        if row.len() > 2 {
            c.push(rule(&to, row));
        }
    }
    c.push(rule(&from, vec![beyond(), counted(&counter(link, "egress"))]));
    let element = |map: &str, data: Value| json!({"add": obj("element", json!({"name": map, "elem": [[link, data]]}))});
    c.push(element(FROM, json!({"jump": {"target": from}})));
    c.push(element(TO, json!({"jump": {"target": to}})));
    if let Some(class) = own.class {
        c.push(element(CLASS, json!(class)));
    }
    c
}

/// The rows of `link`'s chains, as [`hold`] writes them, with the counters of the link that
/// `listed` holds and `own` no longer needs removed.
pub(crate) fn held(link: &str, own: &Own<'_>, listed: &Listed) -> Vec<Value> {
    let wanted = counters(link, own);
    let prefix = format!("{link}_");
    let stale: Vec<String> = listed
        .counters
        .keys()
        .filter(|n| n.starts_with(&prefix) && !wanted.contains(n))
        .cloned()
        .collect();
    hold(link, own, &stale)
}

/// Takes `link` out of the accounting, given what the table holds: its map entries, its
/// chains, and its counters.
pub(crate) fn release(link: &str, listed: &Listed) -> Vec<Value> {
    let mut c = Vec::new();
    let delete =
        |map: &str| json!({"delete": obj("element", json!({"name": map, "elem": [link]}))});
    if listed.linked.iter().any(|l| l == link) {
        c.push(delete(FROM));
        c.push(delete(TO));
    }
    if listed.classes.iter().any(|(l, _)| l == link) {
        c.push(delete(CLASS));
    }
    for name in chains_of(link).iter().filter(|n| listed.chains.contains(n)) {
        c.push(json!({"delete": obj("chain", json!({"name": name}))}));
    }
    let prefix = format!("{link}_");
    for name in listed.counters.keys().filter(|n| n.starts_with(&prefix)) {
        c.push(json!({"delete": obj("counter", json!({"name": name}))}));
    }
    c
}

/// What the host table's accounting holds.
#[derive(Debug, Default, Clone)]
pub(crate) struct Listed {
    /// The links the chain maps name.
    pub(crate) linked: Vec<String>,
    /// Each link's class.
    pub(crate) classes: Vec<(String, u32)>,
    /// The chains of the table.
    pub(crate) chains: Vec<String>,
    /// Each counter, by name, with its bytes and packets.
    pub(crate) counters: std::collections::BTreeMap<String, (u64, u64)>,
}

/// The accounting of a listing of the host table, `nft -j list table inet korpis`'s items.
pub(crate) fn listed(items: &[Value]) -> Listed {
    let mut l = Listed::default();
    for i in items {
        if let Some(name) = i.pointer("/chain/name").and_then(Value::as_str) {
            l.chains.push(name.to_owned());
        }
        if let Some(c) = i.get("counter") {
            let n = |k: &str| c.get(k).and_then(Value::as_u64).unwrap_or(0);
            if let Some(name) = c.get("name").and_then(Value::as_str) {
                l.counters.insert(name.to_owned(), (n("bytes"), n("packets")));
            }
        }
        let map = i.pointer("/map/name").and_then(Value::as_str);
        let elems = i.pointer("/map/elem").and_then(Value::as_array).cloned().unwrap_or_default();
        for e in &elems {
            let link = e.get(0).and_then(Value::as_str).map(str::to_owned);
            match (map, link) {
                (Some(FROM), Some(link)) => l.linked.push(link),
                (Some(CLASS), Some(link)) => {
                    let class =
                        e.get(1).and_then(Value::as_u64).and_then(|c| u32::try_from(c).ok());
                    l.classes.extend(class.map(|c| (link, c)));
                }
                _ => {}
            }
        }
    }
    l
}

/// The counters of every workload's link that `listed` holds, as the meter reads them.
pub(crate) fn read(listed: &Listed) -> Vec<helper::Counter> {
    let prefix = korpis_core::workload::network::HOST_LINK_PREFIX;
    listed
        .counters
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(name, (bytes, packets))| helper::Counter {
            name: name.clone(),
            bytes: *bytes,
            packets: *packets,
            ..Default::default()
        })
        .collect()
}

/// The class `link` holds, or the lowest one free from `first`; none when every class an HTB
/// qdisc can number is held.
pub(crate) fn class_for(link: &str, listed: &Listed, first: u32) -> Option<u32> {
    if let Some((_, c)) = listed.classes.iter().find(|(l, _)| l == link) {
        return Some(*c);
    }
    let used: std::collections::BTreeSet<u32> = listed.classes.iter().map(|(_, c)| *c).collect();
    (first..=0xffff).find(|c| !used.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(port: u16, accounted: bool, ingress: u64) -> Inbound {
        Inbound {
            transport: Transport::Tcp,
            port,
            rules: Vec::new(),
            max_connections: 0,
            new_per_sec: 0,
            shaping: (0, ingress),
            accounted,
            class: 0x11,
            outside: false,
        }
    }

    #[test]
    fn a_workload_is_counted_by_port_and_classed_from_its_link_and_found_again() {
        let ports = [port(25565, true, 0), port(8080, false, 0)];
        let own = Own { ports: &ports, class: Some(0x10), cut_off: false };
        let held = Value::Array(hold("kwabc", &own, &["kwabc_udp_1_in".into()])).to_string();
        assert!(held.contains("\"kwabc_tcp_25565_in\"") && held.contains("\"kwabc_egress\""));
        assert!(!held.contains("8080"), "a port whose accounting is off is not counted");
        assert!(held.contains("\"priority\"") && held.contains("\"4b50:10\""), "{held}");
        assert!(held.contains("\"delete\":{\"counter\""), "a stale counter goes");
        assert!(!held.contains("drop"), "nothing decided but a cut-off");
        let shaped = [port(8080, false, 5_000_000)];
        let own = Own { ports: &shaped, class: None, cut_off: false };
        let held = Value::Array(hold("kwabc", &own, &[])).to_string();
        assert!(held.contains("\"none\"") && held.contains("\"4b51:11\""), "{held}");
        assert!(!held.contains("kwabc_tcp_8080"), "classed, not counted");
        let cut = Own { ports: &ports, class: None, cut_off: true };
        assert!(Value::Array(hold("kwabc", &cut, &[])).to_string().contains("drop"));

        let items = vec![
            json!({"map": {"name": CLASS, "elem": [["kwabc", 16], ["kwdef", 17]]}}),
            json!({"map": {"name": FROM, "elem": [["kwabc", {"jump": {"target": "from_kwabc"}}]]}}),
            json!({"counter": {"name": "kwabc_egress", "bytes": 1200, "packets": 3}}),
            json!({"chain": {"name": "from_kwabc"}}),
        ];
        let l = listed(&items);
        assert_eq!(l.counters.get("kwabc_egress"), Some(&(1200, 3)));
        let read = read(&l);
        assert_eq!((read[0].name.as_str(), read[0].bytes), ("kwabc_egress", 1200), "as metered");
        assert_eq!(class_for("kwabc", &l, 16), Some(16), "its own, kept");
        assert_eq!(class_for("kwnew", &l, 16), Some(18), "the lowest free");
        let released = Value::Array(release("kwabc", &l)).to_string();
        assert!(released.contains("from_kwabc") && released.contains("kwabc_egress"));
    }
}
