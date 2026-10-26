// SPDX-License-Identifier: AGPL-3.0-or-later

//! The nftables rules of a workload's network (§4.1 of docs/07-networking.md), written as the
//! `nft` command's JSON input in one transaction each, so a change is never half applied. The
//! host's table, `inet korpis`, translates what leaves the node from a workload; a workload's own
//! table, in its namespace, drops every new connection in that its endpoints do not declare. The
//! rows of the egress default are added to the host's table by the policy (§6 there).

use super::policy::{Inbound, limited, matches};
use korpis_core::workload::network::RESOLVER_V4;
use serde_json::{Value, json};
use std::io::{self, Write as _};
use std::net::IpAddr;
use std::process::{Command, Stdio};

/// The table Korpis keeps, on the host and in each workload's namespace.
pub(crate) const TABLE: &str = "korpis";

pub(crate) fn chain(name: &str, kind: &str, hook: &str, prio: i32, policy: &str) -> Value {
    json!({"add": {"chain": {
        "family": "inet", "table": TABLE, "name": name,
        "type": kind, "hook": hook, "prio": prio, "policy": policy
    }}})
}

fn rule(chain: &str, expr: Vec<Value>) -> Value {
    let expr = Value::Array(expr);
    json!({"add": {"rule": {"family": "inet", "table": TABLE, "chain": chain, "expr": expr}}})
}

fn flush(chain: &str) -> Value {
    json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": chain}}})
}

/// What every workload's link is named with, as nftables matches a name by its beginning.
const LINKS: &str = "kw*";

/// Masquerade for what comes from a workload's link and leaves by any other but the mesh's: all
/// of IPv4, and IPv6 from unique local addresses, which a routed prefix's addresses are not.
fn masquerade(family: &str, ula_only: bool) -> Value {
    let meta = |key: &str| json!({"meta": {"key": key}});
    let mut expr = vec![
        json!({"match": {"op": "==", "left": meta("nfproto"), "right": family}}),
        json!({"match": {"op": "==", "left": meta("iifname"), "right": LINKS}}),
        json!({"match": {"op": "!=", "left": meta("oifname"), "right": LINKS}}),
        // What leaves into the mesh keeps its source, a workload address its peer allows
        // (§9.1 of docs/07-networking.md).
        json!({"match": {"op": "!=", "left": meta("oifname"), "right": crate::overlay::INTERFACE}}),
    ];
    if ula_only {
        let saddr = json!({"payload": {"protocol": "ip6", "field": "saddr"}});
        let ula = json!({"prefix": {"addr": "fc00::", "len": 7}});
        expr.push(json!({"match": {"op": "==", "left": saddr, "right": ula}}));
    }
    expr.push(json!({"masquerade": null}));
    rule("postrouting", expr)
}

/// The host's table, its translation, and its forwarding, which hold for every workload on the
/// node and name none of them, so they are the same whatever pools the node's agents use.
///
/// It is written only when the table lacks the marker of this version of it, an empty chain
/// named for the version, so the rows' counters survive every workload's network being made;
/// a table of another version has its base chains deleted and written anew, which is how a
/// chain's priority changes, and its counters start again.
pub(crate) fn host(chains: &[String]) -> Vec<Value> {
    let mut commands = vec![json!({"add": {"table": {"family": "inet", "name": TABLE}}})];
    if chains.iter().any(|c| c == BASE) {
        return commands;
    }
    let stale = |c: &&String| BASE_CHAINS.contains(&c.as_str()) || c.starts_with("base_v");
    for c in chains.iter().filter(stale) {
        commands.push(json!({"delete": {"chain": {"family": "inet", "table": TABLE, "name": c}}}));
    }
    commands.extend([
        chain("postrouting", "nat", "postrouting", 100, "accept"),
        flush("postrouting"),
        masquerade("ipv4", false),
        masquerade("ipv6", true),
    ]);
    commands.extend(super::forward::base());
    commands.extend(super::egress::base());
    commands.extend(super::inward::base("workload_forward"));
    commands.extend(super::account::base());
    commands.push(json!({"add": {"chain": {"family": "inet", "table": TABLE, "name": BASE}}}));
    commands
}

/// The marker of this version of the host table's base: raised whenever what [`host`] writes
/// changes, so a node's table is written anew once and then left to count.
const BASE: &str = "base_v4";

/// The chains [`host`] writes, which a table of another version has deleted first.
const BASE_CHAINS: [&str; 8] = [
    "postrouting",
    "forward_prerouting",
    "forward_output",
    "workload_forward",
    "workload_input",
    "account_forward",
    "account_input",
    "account_output",
];

fn accept_when(chain: &str, left: &Value, op: &str, right: &Value) -> Value {
    let matched = json!({"match": {"op": op, "left": left, "right": right}});
    rule(chain, vec![matched, json!({"accept": null})])
}

/// The rules every chain that admits connections begins with: replies, and the messages a
/// network needs.
fn admitted(chain: &str) -> [Value; 3] {
    let l4 = json!({"meta": {"key": "l4proto"}});
    [
        accept_when(
            chain,
            &json!({"ct": {"key": "state"}}),
            "in",
            &json!(["established", "related"]),
        ),
        accept_when(chain, &l4, "==", &json!("icmp")),
        accept_when(chain, &l4, "==", &json!("ipv6-icmp")),
    ]
}

/// A workload's own table, written whole: new connections in are dropped, except over loopback,
/// the messages a network needs, and to the ports `inbound` its endpoints listen on, each through
/// its endpoint's rules and limits where it has any; replies to its own connections are let back.
/// What an endpoint with a limit of its own sends from its port is put in its class on `eth0`
/// (§7 of docs/07-networking.md), by the priority its output chain sets, which the workload,
/// holding no `NET_ADMIN`, cannot change. A priority it sets on a socket itself can put what it
/// sends only in another of its own classes, each beneath its own limit.
///
/// For a microVM, whose guest is behind a tap, the same rules hold on the forward hook, since
/// the guest's connections pass through the namespace rather than end in it, and its priorities
/// are set there; the namespace's own input admits from the guest only the queries its resolver
/// answers, as the guest's kernel is not loopback. A guest is root in its own kernel and can send
/// from any address, so what comes from the tap is taken only from the workload's own addresses,
/// and link-local ones, which the namespace never forwards: anything else is dropped before any
/// other rule, so the guest can neither pass for another workload nor have the resolver answer
/// another address.
pub(crate) fn namespace(inbound: &[Inbound], guest: Option<&Guest<'_>>) -> Value {
    let table = json!({"table": {"family": "inet", "name": TABLE}});
    let mut commands = vec![
        json!({ "add": table.clone() }),
        json!({ "delete": table.clone() }),
        json!({ "add": table }),
        chain("input", "filter", "input", 0, "drop"),
        flush("input"),
        accept_when("input", &json!({"meta": {"key": "iifname"}}), "==", &json!("lo")),
    ];
    if let Some(g) = guest {
        commands.extend(checked(g));
        commands
            .push(rule("input", vec![from_tap(g.tap), json!({"jump": {"target": FROM_GUEST}})]));
    }
    commands.extend(admitted("input"));
    commands.extend([chain("output", "filter", "output", 0, "accept"), flush("output")]);
    let (ports, marks) = if guest.is_some() { ("forward", "forward") } else { ("input", "output") };
    if let Some(g) = guest {
        commands.extend([chain("forward", "filter", "forward", 0, "drop"), flush("forward")]);
        commands
            .push(rule("forward", vec![from_tap(g.tap), json!({"jump": {"target": FROM_GUEST}})]));
        commands.extend(resolved(g.tap));
    }
    for i in inbound.iter().filter(|i| i.shaping.0 > 0) {
        let set = json!({"mangle": {"key": {"meta": {"key": "priority"}},
            "value": super::shape::link_id(i.class)}});
        let [proto, sport] = port_of(i, "sport");
        let mut e: Vec<Value> = guest.iter().map(|g| from_tap(g.tap)).collect();
        e.extend([proto, sport, set]);
        commands.push(rule(marks, e));
    }
    if let Some(g) = guest {
        commands.push(rule("forward", vec![from_tap(g.tap), json!({"accept": null})]));
        commands.extend(admitted("forward"));
    }
    for i in inbound {
        let [proto, dport] = port_of(i, "dport");
        let guarded = !i.rules.is_empty() || i.max_connections > 0 || i.new_per_sec > 0;
        if !guarded {
            commands.push(rule(ports, vec![proto, dport, json!({"accept": null})]));
            continue;
        }
        let name = format!("port_{}_{}", i.transport.name(), i.port);
        commands.push(json!({"add": {"chain": {"family": "inet", "table": TABLE, "name": name}}}));
        commands.push(rule(ports, vec![proto, dport, json!({"jump": {"target": name}})]));
        commands.extend(guard(&name, i));
    }
    json!({"nftables": commands})
}

/// The guest behind a microVM's tap: the tap's name, and the workload's addresses, the only
/// sources the namespace takes from it beside link-local ones.
pub(crate) struct Guest<'a> {
    pub(crate) tap: &'a str,
    pub(crate) addresses: &'a [IpAddr],
}

/// The chain what comes from a guest is checked in before anything else is done with it.
const FROM_GUEST: &str = "from_guest";

fn from_tap(tap: &str) -> Value {
    json!({"match": {"op": "==", "left": {"meta": {"key": "iifname"}}, "right": tap}})
}

/// The chain [`FROM_GUEST`]: back to its caller for the guest's own addresses and link-local
/// ones, and dropped otherwise.
fn checked(g: &Guest<'_>) -> Vec<Value> {
    let saddr = |ip: IpAddr, right: Value| {
        let protocol = if ip.is_ipv4() { "ip" } else { "ip6" };
        let left = json!({"payload": {"protocol": protocol, "field": "saddr"}});
        rule(
            FROM_GUEST,
            vec![
                json!({"match": {"op": "==", "left": left, "right": right}}),
                json!({"return": null}),
            ],
        )
    };
    let mut commands =
        vec![json!({"add": {"chain": {"family": "inet", "table": TABLE, "name": FROM_GUEST}}})];
    commands.extend(g.addresses.iter().map(|ip| saddr(*ip, json!(ip.to_string()))));
    let link_local = json!({"prefix": {"addr": "fe80::", "len": 10}});
    commands.push(saddr(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), link_local));
    commands.push(rule(FROM_GUEST, vec![json!({"counter": null}), json!({"drop": null})]));
    commands
}

/// What the namespace's input admits from a guest behind `tap`, its source already checked: its
/// resolver's queries, over UDP and TCP, to the resolver's address and port.
fn resolved(tap: &str) -> Vec<Value> {
    ["udp", "tcp"]
        .into_iter()
        .map(|proto| {
            rule(
                "input",
                vec![
                    from_tap(tap),
                    json!({"match": {"op": "==", "left": {"payload": {"protocol": "ip",
                    "field": "daddr"}}, "right": RESOLVER_V4.to_string()}}),
                    json!({"match": {"op": "==", "left": {"meta": {"key": "l4proto"}},
                    "right": proto}}),
                    json!({"match": {"op": "==", "left": {"payload": {"protocol": "th",
                    "field": "dport"}}, "right": 53}}),
                    json!({"accept": null}),
                ],
            )
        })
        .collect()
}

/// What matches `i`'s port as `field`, `dport` or `sport`, over its transport.
fn port_of(i: &Inbound, field: &str) -> [Value; 2] {
    [
        json!({"match": {"op": "==", "left": {"meta": {"key": "l4proto"}},
            "right": i.transport.name()}}),
        json!({"match": {"op": "==",
            "left": {"payload": {"protocol": "th", "field": field}}, "right": i.port}}),
    ]
}

/// The rules of the chain `name` for the port `i`: its limits, then its rules in order, then
/// what no rule matched, let in.
fn guard(name: &str, i: &Inbound) -> Vec<Value> {
    let burst = i.new_per_sec.saturating_mul(2);
    let mut rules: Vec<Value> = limited(i.max_connections, (i.new_per_sec, burst))
        .into_iter()
        .map(|(e, comment)| commented(rule(name, e), comment))
        .collect();
    for r in &i.rules {
        let mut e = matches(r, "saddr");
        if r.allow {
            e.push(json!({"accept": null}));
            rules.push(rule(name, e));
        } else {
            e.extend([json!({"counter": null}), json!({"drop": null})]);
            rules.push(commented(rule(name, e), "policy"));
        }
    }
    rules.push(rule(name, vec![json!({"accept": null})]));
    rules
}

fn commented(mut command: Value, comment: &str) -> Value {
    if let Some(r) = command.pointer_mut("/add/rule").and_then(Value::as_object_mut) {
        r.insert("comment".into(), json!(comment));
    }
    command
}

/// Runs `nft` with `ruleset` as its JSON input, in the calling thread's network namespace.
pub(crate) fn apply(ruleset: &Value) -> io::Result<()> {
    let mut child = Command::new("nft")
        .args(["-j", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("nft has no stdin"))?
        .write_all(ruleset.to_string().as_bytes())?;
    let out = child.wait_with_output()?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr);
    Err(io::Error::other(format!("nft refused the rules: {}", why.trim())))
}

/// The base chains on the forward hook, other than Korpis's, that drop what they do not accept:
/// another firewall closing the path every workload's traffic takes. Their names, as
/// `family table chain`.
pub(crate) fn closed_forward() -> io::Result<Vec<String>> {
    let out = Command::new("nft").args(["-j", "list", "chains"]).output()?;
    if !out.status.success() {
        return Err(io::Error::other("nft could not list the chains"));
    }
    let listed: Value = serde_json::from_slice(&out.stdout).map_err(io::Error::other)?;
    let items = listed.get("nftables").and_then(Value::as_array).cloned().unwrap_or_default();
    Ok(items
        .iter()
        .filter_map(|i| i.get("chain"))
        .filter(|c| c.get("hook").and_then(Value::as_str) == Some("forward"))
        .filter(|c| c.get("policy").and_then(Value::as_str) == Some("drop"))
        .filter(|c| c.get("table").and_then(Value::as_str) != Some(TABLE))
        .map(|c| {
            let text = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("?").to_owned();
            format!("{} {} {}", text("family"), text("table"), text("name"))
        })
        .collect())
}
