// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's own policy (§6 of docs/07-networking.md), as the helper holds it: the rules of
//! its intent's egress policy, applied in its chain on the host before the egress default, and
//! each endpoint's rules and limits on who connects to it, applied in its own namespace. Every
//! rule names addresses, because a packet filter judges addresses: a range, or, for a rule
//! naming workloads, the addresses the control plane resolved it to on this node, which are held
//! as a rule for each family that has any, and as none where there are none, since such a rule
//! matches nothing. The control plane refuses the rules a node could not hold before any reaches
//! here.

use super::forward::Transport;
use super::{Prefix, port, range};
use crate::error::{HelperError, HelperResult};
use korpis_core::workload::spec::egress::{
    DEFAULT_BURST, DEFAULT_MAX_CONNECTIONS, DEFAULT_NEW_PER_SEC,
};
use korpis_proto::korpis::helper::v1 as helper;
use serde_json::{Value, json};
use std::net::IpAddr;

/// One rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rule {
    /// Whether it lets the connection on.
    pub(crate) allow: bool,
    peer: Peer,
    ports: Vec<(u16, u16)>,
    transport: Option<Transport>,
}

/// Whom a rule names.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Peer {
    /// An address range.
    Range(Prefix),
    /// Addresses of one family, at least one.
    Addresses(Vec<IpAddr>),
}

/// The peers `r` names: its range, or its addresses, one peer for each family that has any.
fn peers(r: &helper::Rule) -> HelperResult<Vec<Peer>> {
    match (r.cidr.is_empty(), r.addresses.is_empty()) {
        (false, false) => return Err(refuse("a rule names a range or addresses, not both")),
        (false, true) => return Ok(vec![Peer::Range(range("a rule's peer", &r.cidr)?)]),
        _ => {}
    }
    let addresses = r
        .addresses
        .iter()
        .map(|a| a.parse::<IpAddr>().map_err(|_| refuse("a rule's address is an IP address")))
        .collect::<HelperResult<Vec<_>>>()?;
    let (v4, v6): (Vec<IpAddr>, Vec<IpAddr>) = addresses.into_iter().partition(IpAddr::is_ipv4);
    Ok([v4, v6].into_iter().filter(|a| !a.is_empty()).map(Peer::Addresses).collect())
}

/// A port the workload accepts connections on, with who may connect and how many.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Inbound {
    /// Over what.
    pub(crate) transport: Transport,
    /// The port inside the workload.
    pub(crate) port: u16,
    /// Who may connect, in order.
    pub(crate) rules: Vec<Rule>,
    /// Connections at once; zero for no limit of its own.
    pub(crate) max_connections: u32,
    /// New connections a second; zero for no limit of its own.
    pub(crate) new_per_sec: u32,
    /// What it may send from its port and be sent to it, in bits a second; zero for no limit
    /// of its own.
    pub(crate) shaping: (u64, u64),
    /// Whether what it carries is counted.
    pub(crate) accounted: bool,
    /// Its class on the workload's link, where it has a limit of its own: by its place among
    /// the workload's ports, from the first a workload is given.
    pub(crate) class: u32,
    /// Whether it is reached from outside the node at the workload's own address.
    pub(crate) outside: bool,
}

/// The limits on the connections a workload opens, the default's where it sets none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    /// Connections at once, at most the default's.
    pub(crate) max_connections: u32,
    /// New connections a second, at most the default's.
    pub(crate) new_per_sec: u32,
}

impl Limits {
    /// The burst above its rate: the default's, or twice a lowered rate if that is less.
    pub(crate) fn burst(self) -> u32 {
        DEFAULT_BURST.min(self.new_per_sec.saturating_mul(2))
    }
}

fn refuse(m: &str) -> HelperError {
    HelperError::Refused(m.to_owned())
}

fn optional_transport(t: buffa::EnumValue<helper::Transport>) -> HelperResult<Option<Transport>> {
    match t.as_known() {
        Some(helper::Transport::TRANSPORT_UNSPECIFIED) => Ok(None),
        Some(helper::Transport::TRANSPORT_TCP) => Ok(Some(Transport::Tcp)),
        Some(helper::Transport::TRANSPORT_UDP) => Ok(Some(Transport::Udp)),
        None => Err(refuse("a rule's transport is tcp, udp, or any")),
    }
}

/// The rules `rules`, checked.
pub(crate) fn rules(rules: &[helper::Rule]) -> HelperResult<Vec<Rule>> {
    rules
        .iter()
        .map(|r| {
            let ports = r
                .ports
                .iter()
                .map(|p| {
                    let (first, last) =
                        (port("a rule's port", p.first, 1)?, port("a rule's port", p.last, 1)?);
                    if first > last {
                        return Err(refuse("a rule's port range ends before it starts"));
                    }
                    Ok((first, last))
                })
                .collect::<HelperResult<Vec<_>>>()?;
            let transport = optional_transport(r.transport)?;
            let rule = |peer| Rule { allow: r.allow, peer, ports: ports.clone(), transport };
            Ok(peers(r)?.into_iter().map(rule).collect::<Vec<_>>())
        })
        .collect::<HelperResult<Vec<Vec<Rule>>>>()
        .map(|each| each.into_iter().flatten().collect())
}

/// The ports `ports` a workload accepts connections on, checked.
pub(crate) fn inbound(ports: &[helper::Port]) -> HelperResult<Vec<Inbound>> {
    if ports.len() > 0x1_0000 - 0x10 {
        return Err(refuse("more ports than a workload's link has classes for"));
    }
    ports
        .iter()
        .zip(super::shape::FIRST..=0xffff)
        .map(|(p, class)| {
            let transport = optional_transport(p.transport)?
                .ok_or_else(|| refuse("a port's transport is tcp or udp"))?;
            let rules = rules(&p.rules)?;
            if rules.iter().any(|r| !r.ports.is_empty() || r.transport.is_some()) {
                return Err(refuse("an inbound rule is for its own port"));
            }
            Ok(Inbound {
                transport,
                port: port("an inbound port", p.port, 1)?,
                rules,
                max_connections: p.max_connections,
                new_per_sec: p.new_per_sec,
                shaping: (p.egress_limit, p.ingress_limit),
                accounted: p.accounted,
                class,
                outside: p.outside,
            })
        })
        .collect()
}

/// The limits a request sets on the connections its workload opens, which may lower the
/// default's and not raise them.
pub(crate) fn limits(r: &helper::EnsureNetworkRequest) -> HelperResult<Limits> {
    let lowered = |asked: u32, default: u32| {
        if asked > default {
            return Err(refuse("a workload's own limits may only lower the default's"));
        }
        Ok(if asked == 0 { default } else { asked })
    };
    Ok(Limits {
        max_connections: lowered(r.max_connections, DEFAULT_MAX_CONNECTIONS)?,
        new_per_sec: lowered(r.new_per_sec, DEFAULT_NEW_PER_SEC)?,
    })
}

fn matched(left: Value, right: Value) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("op".into(), json!("=="));
    m.insert("left".into(), left);
    m.insert("right".into(), right);
    json!({ "match": m })
}

/// What `r` matches, with the peer's address in the field `field`, `saddr` or `daddr`.
pub(crate) fn matches(r: &Rule, field: &str) -> Vec<Value> {
    let (v4, peer) = match &r.peer {
        Peer::Range(p) => {
            (p.addr.is_ipv4(), json!({"prefix": {"addr": p.addr.to_string(), "len": p.len}}))
        }
        Peer::Addresses(a) => {
            let each: Vec<String> = a.iter().map(ToString::to_string).collect();
            (a.first().is_some_and(IpAddr::is_ipv4), json!({ "set": each }))
        }
    };
    let family = if v4 { "ip" } else { "ip6" };
    let mut m = vec![matched(json!({"payload": {"protocol": family, "field": field}}), peer)];
    if let Some(t) = r.transport {
        m.push(matched(json!({"meta": {"key": "l4proto"}}), json!(t.name())));
    }
    if !r.ports.is_empty() {
        let spans: Vec<Value> = r
            .ports
            .iter()
            .map(|&(a, b)| if a == b { json!(a) } else { json!({"range": [a, b]}) })
            .collect();
        let dport = json!({"payload": {"protocol": "th", "field": "dport"}});
        m.push(matched(dport, json!({ "set": spans })));
    }
    m
}

/// The expressions that drop a new connection past `max` at once, or past `rate` a second
/// with `burst` above it, each counted as `comment`: as rules, with their comments.
pub(crate) fn limited(max: u32, (rate, burst): (u32, u32)) -> Vec<(Vec<Value>, &'static str)> {
    let new = json!({"match": {"op": "in", "left": {"ct": {"key": "state"}}, "right": ["new"]}});
    let dropped = [json!({"counter": null}), json!({"drop": null})];
    let mut rules = Vec::new();
    if rate > 0 {
        let limit = json!({"limit": {"rate": rate, "burst": burst, "per": "second", "inv": true}});
        let mut e = vec![new.clone(), limit];
        e.extend(dropped.clone());
        rules.push((e, "rate"));
    }
    if max > 0 {
        let mut e = vec![new, json!({"ct count": {"val": max, "inv": true}})];
        e.extend(dropped);
        rules.push((e, "limit"));
    }
    rules
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;

    fn named(addresses: &[&str]) -> helper::Rule {
        helper::Rule {
            allow: true,
            addresses: addresses.iter().map(|a| (*a).to_owned()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_rule_naming_workloads_is_a_rule_for_each_family_it_names_and_none_for_none() {
        let both = rules(&[named(&["10.48.4.2", "fd00::2", "10.48.0.2"])]).unwrap();
        assert_eq!(both.len(), 2);
        let v4 = Value::Array(matches(&both[0], "saddr")).to_string();
        assert!(v4.contains(r#""ip""#) && v4.contains("10.48.4.2") && v4.contains("10.48.0.2"));
        let v6 = Value::Array(matches(&both[1], "saddr")).to_string();
        assert!(v6.contains(r#""ip6""#) && v6.contains("fd00::2") && !v6.contains("10.48"));
        assert!(rules(&[named(&[])]).unwrap().is_empty(), "naming none, it matches nothing");
        let mut mixed = named(&["10.48.4.2"]);
        mixed.cidr = "10.0.0.0/8".into();
        assert!(rules(&[mixed]).is_err(), "a range or addresses, not both");
        assert!(rules(&[named(&["not an address"])]).is_err());
        let every = helper::Rule { cidr: "0.0.0.0/0".into(), ..Default::default() };
        let every = Value::Array(matches(&rules(&[every]).unwrap()[0], "saddr")).to_string();
        assert!(every.contains(r#""len":0"#), "every address of a family, as a rule may name");
    }
}
