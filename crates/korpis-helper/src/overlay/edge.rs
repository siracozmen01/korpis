// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node's part in edge groups (§3.1 of docs/07-networking.md). As a member it translates
//! each address, transport, and port of its group's table to its workload, marking what goes to
//! another node so policy routing sends it into that node's GRE link, and refuses every other
//! packet to an address it holds. As a node behind edges it accepts GRE from each edge's mesh
//! address alone, takes from each edge's link only what that edge may reach, marks each such
//! connection with its link, and routes the replies back into it by the mark, so the workload
//! sees its client's own address and the edge reverses its translation.

mod flows;
mod links;

pub(crate) use flows::{listed, sweep};
pub(crate) use links::{Kept, keep};

use crate::error::{HelperError, HelperResult};
use crate::network::nft::{TABLE, chain};
use korpis_proto::korpis::helper::v1 as helper;
use serde_json::{Value, json};
use std::net::Ipv4Addr;

/// The mark of what a member sends into a GRE link, and the table routing it there.
pub(crate) const EDGE_MARK: u32 = 0x4b45_0000;
/// The marks of what arrives from an edge's link, each that link's index beside it.
pub(crate) const SOURCE_MARK: u32 = 0x4b46_0000;
/// What tells Korpis's marks from any other's.
const KORPIS_MARKS: u32 = 0x4b00_0000;
const MARKS_MASK: u32 = 0xff00_0000;
/// The links' names begin so.
pub(crate) const LINKS: &str = "kg";
/// A GRE link inside the mesh: the mesh's MTU less GRE's 24 bytes over IPv4.
pub(crate) const MTU: u32 = super::MTU - 24;

/// The sets and maps the rules name.
const SOURCES: &str = "edge_sources";
const ADMITTED: &str = "edge_admitted";
const SOURCE_MARKS: &str = "edge_marks";
const HELD: &str = "edge_held";
/// The member's chain of translations.
const PREROUTING: &str = "edge_prerouting";
/// What the helper put each held address on, so it removes only what it put there.
pub(crate) const HELD_ON: &str = "edge_held_on";
/// The base chains of the member's side.
const CHAINS: [(&str, &str, &str, i32); 4] = [
    (PREROUTING, "nat", "prerouting", -105),
    ("edge_marks_in", "filter", "prerouting", -150),
    ("edge_marks_out", "route", "output", -150),
    ("edge_input", "filter", "input", 0),
];

/// One address, transport, and port a member translates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Translation {
    pub(crate) address: Ipv4Addr,
    pub(crate) port: u16,
    pub(crate) transport: &'static str,
    pub(crate) via: Option<Ipv4Addr>,
    pub(crate) to: Option<(Ipv4Addr, u16)>,
}

/// One address a member holds, and the interface it is put on; the loopback where none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Hold {
    pub(crate) address: Ipv4Addr,
    pub(crate) interface: String,
}

/// What one edge may reach of this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Admit {
    pub(crate) source: Ipv4Addr,
    pub(crate) destination: Ipv4Addr,
    pub(crate) transport: &'static str,
    pub(crate) port: u16,
}

/// The node's part in edge groups, as asked and checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Table {
    pub(crate) mesh: Option<Ipv4Addr>,
    pub(crate) tunnels: Vec<Ipv4Addr>,
    pub(crate) translations: Vec<Translation>,
    pub(crate) holds: Vec<Hold>,
    pub(crate) admits: Vec<Admit>,
}

/// The name of the link to the node whose mesh address is `remote`.
pub(crate) fn link_name(remote: Ipv4Addr) -> String {
    format!("{LINKS}{:08x}", u32::from(remote))
}

fn transport(t: helper::Transport, what: &str) -> HelperResult<&'static str> {
    match t {
        helper::Transport::TRANSPORT_TCP => Ok("tcp"),
        helper::Transport::TRANSPORT_UDP => Ok("udp"),
        helper::Transport::TRANSPORT_UNSPECIFIED => {
            Err(HelperError::Refused(format!("{what}: no transport")))
        }
    }
}

fn address(text: &str, what: &str) -> HelperResult<Ipv4Addr> {
    text.parse().map_err(|_| HelperError::Refused(format!("{what}: {text:?} is no IPv4 address")))
}

fn optional(text: &str, what: &str) -> HelperResult<Option<Ipv4Addr>> {
    if text.is_empty() { Ok(None) } else { address(text, what).map(Some) }
}

fn port(n: u32, what: &str) -> HelperResult<u16> {
    u16::try_from(n)
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| HelperError::Refused(format!("{what}: {n} is no port")))
}

fn translation(t: &helper::EdgeTranslation) -> HelperResult<Translation> {
    let what = "an edge translation";
    let known = |e: buffa::EnumValue<helper::Transport>| {
        e.as_known().unwrap_or(helper::Transport::TRANSPORT_UNSPECIFIED)
    };
    let to = match optional(&t.to_address, what)? {
        Some(a) => Some((a, port(t.to_port, what)?)),
        None => None,
    };
    Ok(Translation {
        address: address(&t.address, what)?,
        port: port(t.port, what)?,
        transport: transport(known(t.transport), what)?,
        via: optional(&t.via, what)?,
        to,
    })
}

/// What `r` asks of the node's part in edge groups, checked.
///
/// # Errors
/// The first thing it names wrongly, and why.
pub(crate) fn table(r: &helper::KeepOverlayRequest) -> HelperResult<Table> {
    let mesh = optional(&r.mesh_address, "the mesh address")?;
    let Some(e) = r.edges.as_option() else { return Ok(Table { mesh, ..Table::default() }) };
    let tunnels = e.tunnels.iter().map(|t| address(t, "an edge tunnel"));
    let holds = e.holds.iter().map(|h| {
        let interface = if h.interface.is_empty() { "lo".to_owned() } else { h.interface.clone() };
        Ok(Hold { address: address(&h.address, "an edge hold")?, interface })
    });
    let admits = e.admits.iter().map(|a| {
        let what = "an edge admission";
        let known = a.transport.as_known().unwrap_or(helper::Transport::TRANSPORT_UNSPECIFIED);
        Ok(Admit {
            source: address(&a.source, what)?,
            destination: address(&a.destination, what)?,
            transport: transport(known, what)?,
            port: port(a.port, what)?,
        })
    });
    let table = Table {
        mesh,
        tunnels: tunnels.collect::<HelperResult<_>>()?,
        translations: e.translations.iter().map(translation).collect::<HelperResult<_>>()?,
        holds: holds.collect::<HelperResult<_>>()?,
        admits: admits.collect::<HelperResult<_>>()?,
    };
    if table.mesh.is_none() && !table.tunnels.is_empty() {
        return Err(HelperError::Refused("edge tunnels need a mesh address".into()));
    }
    Ok(table)
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
    let daddr = json!({"payload": {"protocol": "ip", "field": "daddr"}});
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
    expr.push(json!({"dnat": {"family": "ip", "addr": to.to_string(), "port": port}}));
    Some(rule(PREROUTING, None, expr))
}

/// The rules the node's part in edge groups adds: its own base chains and sets, written whole;
/// and, for the mesh's chains, the rules ahead of their drops and those after, as
/// `(setup, input_before, forward, input)`.
pub(crate) fn rules(t: &Table, kept: &Kept) -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) {
    let links = &kept.links;
    let mut setup = vec![
        set(SOURCES, json!("ipv4_addr"), None),
        set(ADMITTED, json!(["ifname", "ipv4_addr", "inet_proto", "inet_service"]), None),
        set(SOURCE_MARKS, json!("ifname"), Some("mark")),
        set(HELD, json!("ipv4_addr"), None),
        set(HELD_ON, json!(["ipv4_addr", "ifname"]), None),
    ];
    for (name, kind, hook, prio) in CHAINS {
        setup.push(chain(name, kind, hook, prio, "accept"));
        setup.push(json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": name}}}));
    }
    let sources: Vec<Value> = t.admits.iter().map(|a| json!(a.source.to_string())).collect();
    setup.extend(filled("set", SOURCES, sources));
    let linked = |a: &Ipv4Addr| links.get(a).map(|(name, index)| (name.clone(), *index));
    let admitted = t.admits.iter().filter_map(|a| {
        let (name, _) = linked(&a.source)?;
        Some(json!({"concat": [name, a.destination.to_string(), a.transport, a.port]}))
    });
    setup.extend(filled("set", ADMITTED, admitted.collect()));
    let marks = links.values().map(|(name, index)| json!([name, SOURCE_MARK | (index & 0xffff)]));
    setup.extend(filled("map", SOURCE_MARKS, marks.collect()));
    let held = t.holds.iter().map(|h| json!(h.address.to_string()));
    setup.extend(filled("set", HELD, held.collect()));
    let on = kept.owned.iter().map(|(a, i)| json!({"concat": [a.to_string(), i]}));
    setup.extend(filled("set", HELD_ON, on.collect()));
    setup.extend(t.translations.iter().filter_map(translate));
    for c in ["edge_marks_in", "edge_marks_out"] {
        setup.push(rule(c, None, restore_mark().as_array().cloned().unwrap_or_default()));
    }
    setup.push(rule("edge_marks_in", None, link_mark()));
    let to_held = json!({"match": {"op": "==",
        "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": format!("@{HELD}")}});
    let tcp = meta("==", "l4proto", json!("tcp"));
    setup.push(rule(
        "edge_input",
        Some("edge"),
        vec![to_held.clone(), tcp, json!({"reject": {"type": "tcp reset"}})],
    ));
    setup.push(rule("edge_input", Some("edge"), vec![to_held, json!({"reject": null})]));
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
            meta("!=", "iifname", json!(super::INTERFACE)),
            json!({"counter": null}),
            json!({"drop": null}),
        ],
    )
}

/// The rules of the mesh's chains: GRE from an edge's mesh address, ahead of the mesh's drop;
/// and on the forward and input hooks, what an edge's link may bring, each connection marked
/// with its link, the rest dropped and counted under `edge`.
fn admission() -> (Vec<Value>, Vec<Value>, Vec<Value>) {
    use super::admit::{FORWARD, INPUT};
    let from_mesh = meta("==", "iifname", json!(super::INTERFACE));
    let saddr = json!({"payload": {"protocol": "ip", "field": "saddr"}});
    let gre = vec![
        from_mesh,
        json!({"match": {"op": "==", "left": saddr, "right": format!("@{SOURCES}")}}),
        meta("==", "l4proto", json!("gre")),
        json!({"accept": null}),
    ];
    let from_links = || meta("==", "iifname", json!(format!("{LINKS}*")));
    let key = json!({"concat": [
        {"meta": {"key": "iifname"}},
        {"payload": {"protocol": "ip", "field": "daddr"}},
        {"meta": {"key": "l4proto"}},
        {"payload": {"protocol": "th", "field": "dport"}},
    ]});
    let listed = json!({"match": {"op": "==", "left": key, "right": format!("@{ADMITTED}")}});
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
        vec![
            rule(chain, None, vec![from_links(), replies.clone(), json!({"accept": null})]),
            rule(
                chain,
                Some("edge"),
                [vec![from_links(), listed.clone(), marked.clone()], counted("accept").to_vec()]
                    .concat(),
            ),
            rule(chain, Some("edge"), [vec![from_links()], counted("drop").to_vec()].concat()),
        ]
    };
    let mut forward = vec![rule(FORWARD, None, vec![into_links, syn, clamp])];
    forward.extend(body(FORWARD));
    (vec![rule(INPUT, None, gre)], forward, body(INPUT))
}

#[cfg(test)]
mod tests;
