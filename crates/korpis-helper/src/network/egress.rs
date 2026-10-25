// SPDX-License-Identifier: AGPL-3.0-or-later

//! The egress default (§6 of docs/07-networking.md), on the host's `forward` and `input` hooks
//! for every packet a workload's link sends. Every row that denies counts what it drops, under a
//! comment naming the row, so what this firewall decided is visible apart from what another one
//! on the node decided. Each workload has a chain of its own, reached through a map keyed by its
//! link, holding what differs between workloads: the destinations opened for it, which is the
//! control plane's API for an installed extension's workload, and its new-connection rate.

use super::nft::{TABLE, chain};
use super::policy::{Limits, Rule, limited, matches};
use korpis_core::workload::spec::egress::DEFAULT_MAX_CONNECTIONS;
use serde_json::{Value, json};
use std::net::IpAddr;

/// The map from a workload's link to its own chain.
pub(crate) const OUTWARD: &str = "outward";
/// How many connections a workload may have tracked at once, in both directions.
const TRACKED: u32 = DEFAULT_MAX_CONNECTIONS;
const LINKS: &str = "kw*";
const SMTP: [u16; 3] = [25, 465, 587];
const PRIVATE_V4: [(&str, u8); 5] = [
    ("10.0.0.0", 8),
    ("172.16.0.0", 12),
    ("192.168.0.0", 16),
    ("100.64.0.0", 10),
    ("127.0.0.0", 8),
];
const PRIVATE_V6: [(&str, u8); 2] = [("fc00::", 7), ("::1", 128)];

/// A destination opened for one workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Opened {
    /// Its address.
    pub(crate) addr: IpAddr,
    /// Its port.
    pub(crate) port: u16,
    /// Over what.
    pub(crate) transport: super::forward::Transport,
    /// Why, as the rule's comment says it: `dependency`.
    pub(crate) why: &'static str,
    /// The interface it is opened through alone, where it is: the mesh's, for a target on
    /// another node, so that without a route through the mesh it is not opened at all.
    pub(crate) through: Option<&'static str>,
}

fn rule(chain: &str, comment: Option<&str>, expr: Vec<Value>) -> Value {
    let mut r =
        json!({"family": "inet", "table": TABLE, "chain": chain, "expr": Value::Array(expr)});
    if let (Some(c), Some(o)) = (comment, r.as_object_mut()) {
        o.insert("comment".into(), json!(c));
    }
    json!({"add": {"rule": r}})
}

fn is(left: Value, right: Value) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("op".into(), json!("=="));
    m.insert("left".into(), left);
    m.insert("right".into(), right);
    json!({ "match": m })
}

fn meta(key: &str) -> Value {
    json!({"meta": {"key": key}})
}

fn daddr(protocol: &str) -> Value {
    json!({"payload": {"protocol": protocol, "field": "daddr"}})
}

fn prefixes(list: &[(&str, u8)]) -> Value {
    let items: Vec<Value> =
        list.iter().map(|(a, l)| json!({"prefix": {"addr": a, "len": l}})).collect();
    json!({"set": items})
}

fn state(states: &[&str]) -> Value {
    json!({"match": {"op": "in", "left": {"ct": {"key": "state"}}, "right": states}})
}

fn drop_counted() -> [Value; 2] {
    [json!({"counter": null}), json!({"drop": null})]
}

/// A row: from a workload's link, matching `m`, counted and dropped as `comment`.
fn deny(chain: &str, comment: &str, m: Vec<Value>) -> Value {
    let mut expr = vec![is(meta("iifname"), json!(LINKS))];
    expr.extend(m);
    expr.extend(drop_counted());
    rule(chain, Some(comment), expr)
}

/// Counts the connections of the workload at the address `field` names in `set`, dropping a new
/// one past the limit.
fn tracked(chain: &str, (link, field): (&str, &str), (family, set): (&str, &str)) -> Value {
    let add = json!({"set": {"op": "add", "elem": {"payload": {"protocol": family, "field": field}},
        "set": format!("@{set}"), "stmt": [{"ct count": {"val": TRACKED, "inv": true}}]}});
    let mut expr = vec![is(meta(link), json!(LINKS)), state(&["new"]), add];
    expr.extend(drop_counted());
    rule(chain, Some("conntrack"), expr)
}

/// The rows for what a workload's link sends on through the node.
fn forward_rows(c: &str) -> Vec<Value> {
    let tcp = is(meta("l4proto"), json!("tcp"));
    let smtp = is(json!({"payload": {"protocol": "th", "field": "dport"}}), json!({"set": SMTP}));
    vec![
        rule(
            c,
            None,
            vec![
                is(meta("iifname"), json!(LINKS)),
                state(&["established", "related"]),
                json!({"accept": null}),
            ],
        ),
        tracked(c, ("iifname", "saddr"), ("ip", "conns4")),
        tracked(c, ("iifname", "saddr"), ("ip6", "conns6")),
        tracked(c, ("oifname", "daddr"), ("ip", "conns4")),
        tracked(c, ("oifname", "daddr"), ("ip6", "conns6")),
        rule(
            c,
            None,
            vec![json!({"vmap": {"key": meta("iifname"), "data": format!("@{OUTWARD}")}})],
        ),
        // A connection to a published endpoint is a client's, whoever makes it (§3).
        rule(
            c,
            None,
            vec![
                is(meta("iifname"), json!(LINKS)),
                json!({"match": {"op": "in", "left": {"ct": {"key": "status"}}, "right": "dnat"}}),
                json!({"accept": null}),
            ],
        ),
        deny(c, "other workloads", vec![is(meta("oifname"), json!(LINKS))]),
        deny(
            c,
            "metadata",
            vec![is(daddr("ip"), json!({"prefix": {"addr": "169.254.0.0", "len": 16}}))],
        ),
        deny(
            c,
            "metadata",
            vec![is(daddr("ip6"), json!({"prefix": {"addr": "fe80::", "len": 10}}))],
        ),
        deny(c, "metadata", vec![is(daddr("ip6"), json!("fd00:ec2::254"))]),
        deny(c, "private", vec![is(daddr("ip"), prefixes(&PRIVATE_V4))]),
        deny(c, "private", vec![is(daddr("ip6"), prefixes(&PRIVATE_V6))]),
        deny(c, "smtp", vec![tcp, smtp]),
    ]
}

/// The rows for what a workload's link sends to the node itself: the replies to what it was
/// sent, the messages a network needs, what its own chain opens, and nothing else.
fn input_rows(c: &str) -> Vec<Value> {
    let icmp = json!({"match": {"op": "==", "left": meta("l4proto"), "right": {"set": ["icmp", "ipv6-icmp"]}}});
    vec![
        rule(
            c,
            None,
            vec![
                is(meta("iifname"), json!(LINKS)),
                state(&["established", "related"]),
                json!({"accept": null}),
            ],
        ),
        rule(c, None, vec![is(meta("iifname"), json!(LINKS)), icmp, json!({"accept": null})]),
        rule(
            c,
            None,
            vec![json!({"vmap": {"key": meta("iifname"), "data": format!("@{OUTWARD}")}})],
        ),
        deny(c, "node", Vec::new()),
    ]
}

/// The host's egress default: the sets counting connections, the map to each workload's chain,
/// and the two chains, rewritten whole.
pub(crate) fn base() -> Vec<Value> {
    let set = |name: &str, kind: &str| {
        json!({"add": {"set": {"family": "inet", "table": TABLE, "name": name, "type": kind,
            "size": 65535, "flags": ["dynamic"]}}})
    };
    let mut commands = vec![
        set("conns4", "ipv4_addr"),
        set("conns6", "ipv6_addr"),
        json!({"add": {"map": {"family": "inet", "table": TABLE, "name": OUTWARD,
            "type": "ifname", "map": "verdict"}}}),
    ];
    for (name, hook) in [("workload_forward", "forward"), ("workload_input", "input")] {
        // Before the filter chains at the default priority, an operator's firewall among them:
        // what this one denies is dropped early, and counted, whatever another one does.
        commands.push(chain(name, "filter", hook, -10, "accept"));
        commands
            .push(json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": name}}}));
    }
    commands.extend(forward_rows("workload_forward"));
    commands.extend(input_rows("workload_input"));
    commands
}

/// The chain of the workload whose host link is `link`.
pub(crate) fn chain_of(link: &str) -> String {
    format!("out_{link}")
}

/// What one workload's chain holds: the destinations opened for it, its own rules, and its
/// limits.
pub(crate) struct Own<'a> {
    /// Destinations accepted before anything else.
    pub(crate) opened: &'a [Opened],
    /// Its rules: a deny drops, an allow returns to the default.
    pub(crate) rules: &'a [Rule],
    /// Its limits.
    pub(crate) limits: Limits,
}

/// Makes the chain of the workload with host link `link`: what `own` opens accepted, then its
/// new connections limited, then its own rules; and its entry in the map, given whether it is
/// `listed`.
pub(crate) fn hold(link: &str, own: &Own<'_>, listed: bool) -> Vec<Value> {
    let c = chain_of(link);
    let mut commands = vec![
        json!({"add": {"chain": {"family": "inet", "table": TABLE, "name": c}}}),
        json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": c}}}),
    ];
    for o in own.opened {
        let family = if o.addr.is_ipv4() { "ip" } else { "ip6" };
        let mut expr = vec![
            is(daddr(family), json!(o.addr.to_string())),
            is(meta("l4proto"), json!(o.transport.name())),
            is(json!({"payload": {"protocol": "th", "field": "dport"}}), json!(o.port)),
        ];
        expr.extend(o.through.map(|link| is(meta("oifname"), json!(link))));
        expr.extend([json!({"counter": null}), json!({"accept": null})]);
        commands.push(rule(&c, Some(o.why), expr));
    }
    let l = own.limits;
    let max = if l.max_connections < TRACKED { l.max_connections } else { 0 };
    for (expr, comment) in limited(max, (l.new_per_sec, l.burst())) {
        commands.push(rule(&c, Some(comment), expr));
    }
    for r in own.rules {
        let mut expr = matches(r, "daddr");
        if r.allow {
            expr.push(json!({"return": null}));
            commands.push(rule(&c, None, expr));
        } else {
            expr.extend(drop_counted());
            commands.push(rule(&c, Some("policy"), expr));
        }
    }
    if !listed {
        let elem = json!([link, {"jump": {"target": c}}]);
        commands.push(json!({"add": {"element": {"family": "inet", "table": TABLE,
            "name": OUTWARD, "elem": [elem]}}}));
    }
    commands
}

/// Takes the workload with host link `link` out of the egress default: its map entry, if
/// `listed`, and its chain, if `chained`.
pub(crate) fn release(link: &str, (listed, chained): (bool, bool)) -> Vec<Value> {
    let mut commands = Vec::new();
    if listed {
        commands.push(json!({"delete": {"element": {"family": "inet", "table": TABLE,
            "name": OUTWARD, "elem": [link]}}}));
    }
    if chained {
        commands.push(json!({"delete": {"chain": {"family": "inet", "table": TABLE,
            "name": chain_of(link)}}}));
    }
    commands
}
