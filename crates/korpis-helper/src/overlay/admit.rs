// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the mesh lets into the node (§9.1 of docs/07-networking.md), in the host's table: the
//! replies to what the node's workloads opened through it; each dependent on another node to the
//! endpoint it depends on, counted under `dependency`; each node sending this one's replicas to
//! the node's own mesh address on the transfer port, counted under `transfer`; and nothing else,
//! to a workload or to the node itself, counted under `overlay` as it is dropped. WireGuard has already authenticated each
//! packet's source as the node holding its pool, so the rules name addresses. What enters the
//! mesh has its TCP segment size clamped to the route's MTU, which is the interface's.

use super::INTERFACE;
use crate::error::{HelperError, HelperResult};
use crate::network::nft::{TABLE, chain};
use korpis_proto::korpis::helper::v1 as helper;
use serde_json::{Value, json};
use std::net::Ipv4Addr;

/// The chain on the forward hook, and the one on the input hook.
pub(crate) const FORWARD: &str = "overlay_forward";
pub(crate) const INPUT: &str = "overlay_input";
/// The set of what is admitted: source, destination, transport, port.
const SET: &str = "overlay_admitted";
/// The set of the nodes admitted to the transfer port.
const SENDERS: &str = "overlay_transfer";
/// Before the egress default, so what this drops is counted here.
const PRIORITY: i32 = -15;

/// One dependent admitted to one endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Admitted {
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
    pub transport: &'static str,
    pub port: u16,
}

/// What `r` admits, checked: addresses that are IPv4 addresses, a port, and a transport.
///
/// # Errors
/// The first that is not, and why.
pub(crate) fn admitted(r: &helper::KeepOverlayRequest) -> HelperResult<Vec<Admitted>> {
    let refused = |why: &str| HelperError::Refused(format!("an admitted dependent's {why}"));
    r.admitted
        .iter()
        .map(|a| {
            let address = |text: &str| text.parse::<Ipv4Addr>().map_err(|_| refused("address"));
            let transport = match a.transport.as_known() {
                Some(helper::Transport::TRANSPORT_TCP) => "tcp",
                Some(helper::Transport::TRANSPORT_UDP) => "udp",
                _ => return Err(refused("transport")),
            };
            let port = u16::try_from(a.port).ok().filter(|p| *p > 0).ok_or(refused("port"))?;
            Ok(Admitted {
                source: address(&a.source)?,
                destination: address(&a.destination)?,
                transport,
                port,
            })
        })
        .collect()
}

/// The nodes admitted to send replicas to this one: their mesh addresses, to this node's own on
/// one TCP port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Transfers {
    pub sources: Vec<Ipv4Addr>,
    pub to: Ipv4Addr,
    pub port: u16,
}

/// The transfers `r` admits, checked; none when it names no source.
///
/// # Errors
/// A source that is no IPv4 address, or sources with no mesh address or port to admit them to.
pub(crate) fn transfers(r: &helper::KeepOverlayRequest) -> HelperResult<Option<Transfers>> {
    if r.transfer_sources.is_empty() {
        return Ok(None);
    }
    let refused = |why: &str| HelperError::Refused(format!("the transfers' {why}"));
    let sources = r
        .transfer_sources
        .iter()
        .map(|s| s.parse::<Ipv4Addr>().map_err(|_| refused("source")))
        .collect::<HelperResult<Vec<_>>>()?;
    let to = r.mesh_address.parse::<Ipv4Addr>().map_err(|_| refused("mesh address"))?;
    let port = u16::try_from(r.transfer_port).ok().filter(|p| *p > 0).ok_or(refused("port"))?;
    Ok(Some(Transfers { sources, to, port }))
}

/// The rule admitting `t`'s sources, and the commands that fill their set.
fn transfer_rules(t: Option<&Transfers>) -> (Vec<Value>, Option<Value>) {
    let mut setup = vec![
        json!({"add": {"set": {"family": "inet", "table": TABLE, "name": SENDERS,
            "type": "ipv4_addr"}}}),
        json!({"flush": {"set": {"family": "inet", "table": TABLE, "name": SENDERS}}}),
    ];
    let Some(t) = t else { return (setup, None) };
    let elem: Vec<String> = t.sources.iter().map(Ipv4Addr::to_string).collect();
    setup.push(json!({"add": {"element": {"family": "inet", "table": TABLE, "name": SENDERS,
        "elem": elem}}}));
    let field = |protocol: &str, field: &str, right: Value| {
        json!({"match": {"op": "==",
            "left": {"payload": {"protocol": protocol, "field": field}}, "right": right}})
    };
    let expr = vec![
        is("iifname", INTERFACE),
        field("ip", "saddr", json!(format!("@{SENDERS}"))),
        field("ip", "daddr", json!(t.to.to_string())),
        field("tcp", "dport", json!(t.port)),
        json!({"counter": null}),
        json!({"accept": null}),
    ];
    (setup, Some(rule(INPUT, Some("transfer"), expr)))
}

fn rule(chain: &str, comment: Option<&str>, expr: Vec<Value>) -> Value {
    let mut r =
        json!({"family": "inet", "table": TABLE, "chain": chain, "expr": Value::Array(expr)});
    if let (Some(c), Some(o)) = (comment, r.as_object_mut()) {
        o.insert("comment".into(), json!(c));
    }
    json!({"add": {"rule": r}})
}

fn is(key: &str, right: &str) -> Value {
    json!({"match": {"op": "==", "left": {"meta": {"key": key}}, "right": right}})
}

fn replies() -> Value {
    json!({"match": {"op": "in", "left": {"ct": {"key": "state"}},
        "right": ["established", "related"]}})
}

/// The rules edges add (§3.1 of docs/07-networking.md): their own chains, sets, and rules; the
/// rules of the mesh's input chain ahead of its drop; and those of its forward and input chains
/// for what arrives by an edge's link.
#[derive(Debug, Clone, Default)]
pub(crate) struct EdgeRules {
    pub(crate) setup: Vec<Value>,
    pub(crate) input_before: Vec<Value>,
    pub(crate) forward: Vec<Value>,
    pub(crate) input: Vec<Value>,
}

/// Every rule of the mesh's admission, written whole in one transaction: the chains and the sets
/// made if missing and emptied, then filled with what `admitted` and `transfers` name, with what
/// `edges` adds.
pub(crate) fn rules(
    admitted: &[Admitted],
    transfers: Option<&Transfers>,
    edges: EdgeRules,
) -> Value {
    let from = || is("iifname", INTERFACE);
    let mut commands = vec![
        json!({"add": {"table": {"family": "inet", "name": TABLE}}}),
        chain(FORWARD, "filter", "forward", PRIORITY, "accept"),
        chain(INPUT, "filter", "input", PRIORITY, "accept"),
        json!({"add": {"set": {"family": "inet", "table": TABLE, "name": SET,
            "type": ["ipv4_addr", "ipv4_addr", "inet_proto", "inet_service"]}}}),
        json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": FORWARD}}}),
        json!({"flush": {"chain": {"family": "inet", "table": TABLE, "name": INPUT}}}),
        json!({"flush": {"set": {"family": "inet", "table": TABLE, "name": SET}}}),
    ];
    if !admitted.is_empty() {
        let elem: Vec<Value> = admitted
            .iter()
            .map(|a| {
                json!({"concat": [a.source.to_string(), a.destination.to_string(),
                    a.transport, a.port]})
            })
            .collect();
        commands.push(json!({"add": {"element": {"family": "inet", "table": TABLE,
            "name": SET, "elem": elem}}}));
    }
    let syn = json!({"match": {"op": "in",
        "left": {"payload": {"protocol": "tcp", "field": "flags"}}, "right": "syn"}});
    let clamp = json!({"mangle": {"key": {"tcp option": {"name": "maxseg", "field": "size"}},
        "value": {"rt": {"key": "mtu"}}}});
    let key = json!({"concat": [
        {"payload": {"protocol": "ip", "field": "saddr"}},
        {"payload": {"protocol": "ip", "field": "daddr"}},
        {"meta": {"key": "l4proto"}},
        {"payload": {"protocol": "th", "field": "dport"}},
    ]});
    let listed = json!({"match": {"op": "==", "left": key, "right": format!("@{SET}")}});
    let counted = |verdict: &str| [json!({"counter": null}), json!({ verdict: null })];
    let (senders, transfer) = transfer_rules(transfers);
    commands.extend(senders);
    commands.extend(edges.setup);
    commands.extend([
        rule(FORWARD, None, vec![is("oifname", INTERFACE), syn, clamp]),
        rule(FORWARD, None, vec![from(), replies(), json!({"accept": null})]),
        rule(
            FORWARD,
            Some("dependency"),
            [vec![from(), listed], counted("accept").to_vec()].concat(),
        ),
        rule(FORWARD, Some("overlay"), [vec![from()], counted("drop").to_vec()].concat()),
        rule(INPUT, None, vec![from(), replies(), json!({"accept": null})]),
    ]);
    commands.extend(transfer);
    commands.extend(edges.input_before);
    commands.push(rule(INPUT, Some("overlay"), [vec![from()], counted("drop").to_vec()].concat()));
    commands.extend(edges.forward);
    commands.extend(edges.input);
    json!({"nftables": commands})
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    #[test]
    fn what_is_admitted_is_checked_and_listed_and_all_else_from_the_mesh_dropped() {
        let request = |port: u32, transport: helper::Transport| helper::KeepOverlayRequest {
            admitted: vec![helper::OverlayAdmitted {
                source: "10.48.4.2".into(),
                destination: "10.48.0.3".into(),
                port,
                transport: transport.into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let asked = admitted(&request(5432, helper::Transport::TRANSPORT_TCP)).unwrap();
        assert_eq!((asked[0].transport, asked[0].port), ("tcp", 5432));
        assert!(admitted(&request(0, helper::Transport::TRANSPORT_TCP)).is_err());
        assert!(admitted(&request(5432, helper::Transport::TRANSPORT_UNSPECIFIED)).is_err());
        let text = rules(&asked, None, EdgeRules::default()).to_string();
        assert!(text.contains(r#"{"concat":["10.48.4.2","10.48.0.3","tcp",5432]}"#), "{text}");
        assert!(text.contains(r#""comment":"overlay""#) && text.contains(r#""drop":null"#));
        let none = rules(&[], None, EdgeRules::default()).to_string();
        assert!(!none.contains(r#""element""#), "none listed, none added");
        assert!(none.contains(r#""flush":{"set""#) && !none.contains(r#""transfer""#));
    }

    #[test]
    fn a_sender_is_admitted_to_the_transfer_port_of_the_node_s_own_mesh_address_alone() {
        let mut r = helper::KeepOverlayRequest {
            transfer_sources: vec!["10.48.1.255".into()],
            transfer_port: 51820,
            ..Default::default()
        };
        assert!(transfers(&r).is_err(), "nowhere to admit it to");
        r.mesh_address = "10.48.0.255".into();
        let t = transfers(&r).unwrap().unwrap();
        let text = rules(&[], Some(&t), EdgeRules::default()).to_string();
        assert!(text.contains(r#""elem":["10.48.1.255"]"#), "{text}");
        assert!(text.contains(r#""right":"10.48.0.255""#) && text.contains(r#""right":51820"#));
        assert!(text.contains(r#""comment":"transfer""#));
        let transfer = text.find(r#""comment":"transfer""#).unwrap_or(usize::MAX);
        let dropped = text.rfind(r#""comment":"overlay""#).unwrap_or(0);
        assert!(transfer < dropped, "before what the mesh drops");
        r.transfer_sources = vec!["no address".into()];
        assert!(transfers(&r).is_err());
        r.transfer_sources.clear();
        assert_eq!(transfers(&r).unwrap(), None);
    }
}
