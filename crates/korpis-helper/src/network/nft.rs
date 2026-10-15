// SPDX-License-Identifier: AGPL-3.0-or-later

//! The nftables rules of a workload's network (§4.1 of docs/07-networking.md), written as the
//! `nft` command's JSON input in one transaction each, so a change is never half applied. The
//! host's table, `inet korpis`, translates what leaves the node from a workload; a workload's own
//! table, in its namespace, drops every new connection in that its endpoints do not declare. The
//! rows of the egress default are added to the host's table by the policy (§6 there).

use serde_json::{Value, json};
use std::io::{self, Write as _};
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

/// Masquerade for what comes from a workload's link and leaves by any other: all of IPv4, and
/// IPv6 from unique local addresses, which a routed prefix's addresses are not.
fn masquerade(family: &str, ula_only: bool) -> Value {
    let meta = |key: &str| json!({"meta": {"key": key}});
    let mut expr = vec![
        json!({"match": {"op": "==", "left": meta("nfproto"), "right": family}}),
        json!({"match": {"op": "==", "left": meta("iifname"), "right": LINKS}}),
        json!({"match": {"op": "!=", "left": meta("oifname"), "right": LINKS}}),
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
pub(crate) fn host() -> Vec<Value> {
    let mut commands = vec![
        json!({"add": {"table": {"family": "inet", "name": TABLE}}}),
        chain("postrouting", "nat", "postrouting", 100, "accept"),
        flush("postrouting"),
        masquerade("ipv4", false),
        masquerade("ipv6", true),
    ];
    commands.extend(super::forward::base());
    commands
}

fn accept_when(left: &Value, op: &str, right: &Value) -> Value {
    let matched = json!({"match": {"op": op, "left": left, "right": right}});
    rule("input", vec![matched, json!({"accept": null})])
}

/// A workload's own table: new connections in are dropped, except over loopback, the messages a
/// network needs, and to the ports `inbound` its endpoints listen on; replies to its own
/// connections are let back.
pub(crate) fn namespace(inbound: &[(&str, u16)]) -> Value {
    let mut commands = vec![
        json!({"add": {"table": {"family": "inet", "name": TABLE}}}),
        chain("input", "filter", "input", 0, "drop"),
        flush("input"),
        accept_when(&json!({"meta": {"key": "iifname"}}), "==", &json!("lo")),
        accept_when(&json!({"ct": {"key": "state"}}), "in", &json!(["established", "related"])),
        accept_when(&json!({"meta": {"key": "l4proto"}}), "==", &json!("icmp")),
        accept_when(&json!({"meta": {"key": "l4proto"}}), "==", &json!("ipv6-icmp")),
    ];
    for (transport, port) in inbound {
        let proto = json!({"match": {"op": "==", "left": {"meta": {"key": "l4proto"}},
            "right": transport}});
        let dport = json!({"match": {"op": "==",
            "left": {"payload": {"protocol": "th", "field": "dport"}}, "right": port}});
        commands.push(rule("input", vec![proto, dport, json!({"accept": null})]));
    }
    json!({"nftables": commands})
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
