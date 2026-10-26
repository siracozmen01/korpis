// SPDX-License-Identifier: AGPL-3.0-or-later

//! The nftables rules of the node's part in edge groups (§3.1 of docs/07-networking.md): its own
//! base chains and sets, written whole, and the rules of the mesh's chains. Every address of a
//! member's or a node's part may be IPv4 or IPv6, and each family has sets of its own, since an
//! nftables set holds one; the GRE links that carry both are IPv4 whatever they carry.

use super::{
    ADMITTED, CHAINS, EDGE_MARK, HELD, HELD_ON, KORPIS_MARKS, Kept, LINKS, MARKS_MASK, PREROUTING,
    SOURCE_MARK, SOURCE_MARKS, SOURCES, Table, Translation,
};
use crate::network::nft::{TABLE, chain};
use serde_json::{Value, json};
use std::net::{IpAddr, Ipv4Addr};

/// The payload protocol naming an address of `a`'s family.
fn family(a: IpAddr) -> &'static str {
    if a.is_ipv4() { "ip" } else { "ip6" }
}

/// `object` with `value` put at `key`.
fn with(mut object: Value, key: &str, value: Value) -> Value {
    if let Some(o) = object.as_object_mut() {
        o.insert(key.into(), value);
    }
    object
}

fn meta(op: &str, key: &str, right: Value) -> Value {
    json!({"match": with(json!({"op": op, "left": {"meta": {"key": key}}}), "right", right)})
}

fn rule(chain: &str, comment: Option<&str>, expr: Vec<Value>) -> Value {
    let mut r =
        json!({"family": "inet", "table": TABLE, "chain": chain, "expr": Value::Array(expr)});
    if let (Some(c), Some(o)) = (comment, r.as_object_mut()) {
        o.insert("comment".into(), json!(c));
    }
    json!({"add": {"rule": r}})
}

fn set(name: &str, kind: Value, map: Option<&str>) -> Value {
    let mut s = with(json!({"family": "inet", "table": TABLE, "name": name}), "type", kind);
    let what = match (map, s.as_object_mut()) {
        (Some(data), Some(o)) => {
            o.insert("map".into(), json!(data));
            "map"
        }
        _ => "set",
    };
    json!({"add": {what: s}})
}

fn filled(kind: &str, name: &str, elem: Vec<Value>) -> Vec<Value> {
    let mut out = vec![json!({"flush": {kind: {"family": "inet", "table": TABLE, "name": name}}})];
    if !elem.is_empty() {
        let element = json!({"family": "inet", "table": TABLE, "name": name});
        out.push(json!({"add": {"element": with(element, "elem", Value::Array(elem))}}));
    }
    out
}

/// Korpis's marks, moved from the connection onto each of its packets, so every packet of a flow
/// is routed as its first was.
fn restore_mark() -> Value {
    let masked = json!({"&": [{"ct": {"key": "mark"}}, MARKS_MASK]});
    json!([{"match": {"op": "==", "left": masked, "right": KORPIS_MARKS}},
        {"mangle": {"key": {"meta": {"key": "mark"}}, "value": {"ct": {"key": "mark"}}}}])
}

/// A packet arriving through a link with no mark of Korpis's on its connection is given its
/// link's, for the kernel's check of its source to find its way back into the link; once its
/// connection is admitted, the connection carries the same mark.
fn link_mark() -> Vec<Value> {
    let masked = json!({"&": [{"meta": {"key": "mark"}}, MARKS_MASK]});
    vec![
        meta("==", "iifname", json!(format!("{LINKS}*"))),
        json!({"match": {"op": "!=", "left": masked, "right": KORPIS_MARKS}}),
        json!({"mangle": {"key": {"meta": {"key": "mark"}}, "value": {"map": {
            "key": {"meta": {"key": "iifname"}}, "data": format!("@{SOURCE_MARKS}")}}}}),
    ]
}

fn translate(t: &Translation) -> Option<Value> {
    let (to, port) = t.to?;
    let daddr = json!({"payload": {"protocol": family(t.address), "field": "daddr"}});
    let dport = json!({"payload": {"protocol": "th", "field": "dport"}});
    let mut expr = vec![
        json!({"match": {"op": "==", "left": daddr, "right": t.address.to_string()}}),
        meta("==", "l4proto", json!(t.transport)),
        json!({"match": {"op": "==", "left": dport, "right": t.port}}),
    ];
    if t.via.is_some() {
        let mark = |key: Value| json!({"mangle": {"key": key, "value": EDGE_MARK}});
        expr.push(mark(json!({"meta": {"key": "mark"}})));
        expr.push(mark(json!({"ct": {"key": "mark"}})));
    }
    expr.push(json!({"dnat": {"family": family(to), "addr": to.to_string(), "port": port}}));
    Some(rule(PREROUTING, None, expr))
}

/// The sets of each family, filled: what each edge's link may reach here, the addresses held,
/// and those the helper put on an interface.
fn family_sets(t: &Table, kept: &Kept) -> Vec<Value> {
    let mut out = Vec::new();
    let linked = |a: &Ipv4Addr| kept.links.get(a).map(|(name, _)| name.clone());
    for (i, v4) in [true, false].into_iter().enumerate() {
        let one = |names: [&'static str; 2]| names.get(i).copied().unwrap_or_default();
        let ours = |a: &IpAddr| a.is_ipv4() == v4;
        let admitted = t.admits.iter().filter(|a| ours(&a.destination)).filter_map(|a| {
            let name = linked(&a.source)?;
            Some(json!({"concat": [name, a.destination.to_string(), a.transport, a.port]}))
        });
        out.extend(filled("set", one(ADMITTED), admitted.collect()));
        let held = t.holds.iter().filter(|h| ours(&h.address));
        let held = held.map(|h| json!(h.address.to_string()));
        out.extend(filled("set", one(HELD), held.collect()));
        let on = kept.owned.iter().filter(|(a, _)| ours(a));
        let on = on.map(|(a, i)| json!({"concat": [a.to_string(), i]}));
        out.extend(filled("set", one(HELD_ON), on.collect()));
    }
    out
}

/// What refuses every packet to a held address that no translation took: a TCP reset, an ICMP
/// port unreachable otherwise; but a neighbour's unicast solicitation of an IPv6 one, which
/// confirms the address is still reachable, is answered, since refused, the neighbour would stop
/// sending to it.
fn held_refusals() -> Vec<Value> {
    let mut out = Vec::new();
    for (protocol, held) in [("ip", HELD[0]), ("ip6", HELD[1])] {
        let to_held = json!({"match": {"op": "==",
            "left": {"payload": {"protocol": protocol, "field": "daddr"}},
            "right": format!("@{held}")}});
        if protocol == "ip6" {
            let discovery = json!({"match": {"op": "==",
                "left": {"payload": {"protocol": "icmpv6", "field": "type"}},
                "right": {"set": ["nd-neighbor-solicit", "nd-neighbor-advert"]}}});
            let answered = vec![to_held.clone(), discovery, json!({"accept": null})];
            out.push(rule("edge_input", None, answered));
        }
        let tcp = meta("==", "l4proto", json!("tcp"));
        let reset = vec![to_held.clone(), tcp, json!({"reject": {"type": "tcp reset"}})];
        out.push(rule("edge_input", Some("edge"), reset));
        out.push(rule("edge_input", Some("edge"), vec![to_held, json!({"reject": null})]));
    }
    out
}

/// The rules the node's part in edge groups adds: its own base chains and sets, written whole;
/// and, for the mesh's chains, the rules ahead of their drops and those after, as
/// `(setup, input_before, forward, input)`.
pub(crate) fn rules(t: &Table, kept: &Kept) -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) {
    let mut setup = vec![
        set(SOURCES, json!("ipv4_addr"), None),
        set(SOURCE_MARKS, json!("ifname"), Some("mark")),
    ];
    for (i, kind) in ["ipv4_addr", "ipv6_addr"].into_iter().enumerate() {
        let one = |names: [&'static str; 2]| names.get(i).copied().unwrap_or_default();
        setup.push(set(one(ADMITTED), json!(["ifname", kind, "inet_proto", "inet_service"]), None));
        setup.push(set(one(HELD), json!(kind), None));
        setup.push(set(one(HELD_ON), json!([kind, "ifname"]), None));
    }
    for (name, kind, hook, prio) in CHAINS {
        setup.push(chain(name, kind, hook, prio, "accept"));
        setup.push(json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": name}}}));
    }
    let sources: Vec<Value> = t.admits.iter().map(|a| json!(a.source.to_string())).collect();
    setup.extend(filled("set", SOURCES, sources));
    setup.extend(family_sets(t, kept));
    let marks =
        kept.links.values().map(|(name, index)| json!([name, SOURCE_MARK | (index & 0xffff)]));
    setup.extend(filled("map", SOURCE_MARKS, marks.collect()));
    setup.extend(t.translations.iter().filter_map(translate));
    for c in ["edge_marks_in", "edge_marks_out"] {
        setup.push(rule(c, None, restore_mark().as_array().cloned().unwrap_or_default()));
    }
    setup.push(rule("edge_marks_in", None, link_mark()));
    setup.extend(held_refusals());
    setup.extend(t.mesh.map(outside_mesh));
    let (input_before, forward, input) = admission();
    (setup, input_before, forward, input)
}

/// GRE to this node's mesh address arriving other than through the mesh, dropped and counted:
/// the kernel gives a GRE packet to the link its outer addresses name, whatever interface it came
/// by, so a machine on the node's own network could otherwise send into an edge's link with that
/// edge's mesh address as its source. Through the mesh, WireGuard has authenticated it.
fn outside_mesh(mesh: Ipv4Addr) -> Value {
    let daddr = json!({"payload": {"protocol": "ip", "field": "daddr"}});
    rule(
        "edge_input",
        Some("edge"),
        vec![
            meta("==", "l4proto", json!("gre")),
            json!({"match": {"op": "==", "left": daddr, "right": mesh.to_string()}}),
            meta("!=", "iifname", json!(crate::overlay::INTERFACE)),
            json!({"counter": null}),
            json!({"drop": null}),
        ],
    )
}

/// The rules of the mesh's chains: GRE from an edge's mesh address, ahead of the mesh's drop;
/// and on the forward and input hooks, what an edge's link may bring, each connection marked
/// with its link, the rest dropped and counted under `edge`.
fn admission() -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    use crate::overlay::admit::{FORWARD, INPUT};
    let from_mesh = meta("==", "iifname", json!(crate::overlay::INTERFACE));
    let saddr = json!({"payload": {"protocol": "ip", "field": "saddr"}});
    let gre = vec![
        from_mesh,
        json!({"match": {"op": "==", "left": saddr, "right": format!("@{SOURCES}")}}),
        meta("==", "l4proto", json!("gre")),
        json!({"accept": null}),
    ];
    let from_links = || meta("==", "iifname", json!(format!("{LINKS}*")));
    let listed = |(protocol, admitted): (&str, &str)| {
        let key = json!({"concat": [
            {"meta": {"key": "iifname"}},
            {"payload": {"protocol": protocol, "field": "daddr"}},
            {"meta": {"key": "l4proto"}},
            {"payload": {"protocol": "th", "field": "dport"}},
        ]});
        json!({"match": {"op": "==", "left": key, "right": format!("@{admitted}")}})
    };
    let marked = json!({"mangle": {"key": {"ct": {"key": "mark"}}, "value": {"map": {
        "key": {"meta": {"key": "iifname"}}, "data": format!("@{SOURCE_MARKS}")}}}});
    let replies = json!({"match": {"op": "in", "left": {"ct": {"key": "state"}},
        "right": ["established", "related"]}});
    let syn = json!({"match": {"op": "in",
        "left": {"payload": {"protocol": "tcp", "field": "flags"}}, "right": "syn"}});
    let clamp = json!({"mangle": {"key": {"tcp option": {"name": "maxseg", "field": "size"}},
        "value": {"rt": {"key": "mtu"}}}});
    let into_links = meta("==", "oifname", json!(format!("{LINKS}*")));
    let counted = |verdict: &str| [json!({"counter": null}), json!({ verdict: null })];
    let body = |chain: &str| {
        let mut out =
            vec![rule(chain, None, vec![from_links(), replies.clone(), json!({"accept": null})])];
        for of in [("ip", ADMITTED[0]), ("ip6", ADMITTED[1])] {
            let admitted = vec![from_links(), listed(of), marked.clone()];
            out.push(rule(chain, Some("edge"), [admitted, counted("accept").to_vec()].concat()));
        }
        out.push(rule(
            chain,
            Some("edge"),
            [vec![from_links()], counted("drop").to_vec()].concat(),
        ));
        out
    };
    let mut forward = vec![rule(FORWARD, None, vec![into_links, syn, clamp])];
    forward.extend(body(FORWARD));
    (vec![rule(INPUT, None, gre)], forward, body(INPUT))
}
