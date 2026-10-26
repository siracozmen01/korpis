// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rules naming workloads (§6 of docs/07-networking.md): the addresses a rule naming workloads by
//! a selector names on the node of the workload whose rule it is. A packet filter judges
//! addresses, so the control plane resolves each such rule to the addresses of the workloads of
//! the project it matches, as their nodes observed them, and names an address only where the
//! rule's node routes a packet for it to the matched workload: one on the rule's own node, at its
//! addresses there, and one on another member of the mesh, at its addresses in that member's
//! routed pools, which no other member's overlap, where the rule's node is a member too. Any
//! other private address may be one the rule's node gave a workload of its own, so naming it
//! would judge another workload. A deny names, besides, every global IPv6 address a matched
//! workload is observed at, which reaches an endpoint exposed at a global address from anywhere:
//! naming it drops more, and never lets more in.

use super::spec::IntentBody;
use super::spec::endpoints::{NetworkAction, NetworkRule, Peer};
use crate::labels::LabelSelector;
use ipnet::IpNet;
use std::collections::BTreeMap;
use std::net::IpAddr;
use uuid::Uuid;

/// Where a rule is: an endpoint's, by name, or the workload's own egress; and its place among
/// that policy's rules.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RulePlace {
    /// The endpoint whose rule it is; none for the workload's egress.
    pub endpoint: Option<String>,
    /// Its place among the policy's rules, from zero.
    pub index: u32,
}

/// A rule naming workloads: where it is, whether it allows, and what it selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Naming<'a> {
    /// Where it is.
    pub place: (Option<&'a str>, u32),
    /// Whether it allows; otherwise it denies.
    pub allow: bool,
    /// The workloads it names.
    pub selector: &'a LabelSelector,
}

fn naming<'a>(endpoint: Option<&'a str>, rules: &'a [NetworkRule]) -> Vec<Naming<'a>> {
    rules
        .iter()
        .zip(0_u32..)
        .filter_map(|(r, index)| match &r.peer {
            Peer::Workloads(selector) => Some(Naming {
                place: (endpoint, index),
                allow: r.action == NetworkAction::Allow,
                selector,
            }),
            _ => None,
        })
        .collect()
}

/// Every rule of `body` that names workloads: each endpoint's, then its egress's.
#[must_use]
pub fn naming_workloads(body: &IntentBody) -> Vec<Naming<'_>> {
    let mut out: Vec<Naming<'_>> = body
        .endpoints
        .iter()
        .flat_map(|e| naming(Some(e.name.as_str()), &e.policy.ingress))
        .collect();
    out.extend(naming(None, &body.egress.rules));
    out
}

/// A workload of the project a selector may match: its labels, the node it is placed on, and
/// the addresses that node observed it at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located<'a> {
    /// The workload.
    pub id: Uuid,
    /// Its labels.
    pub labels: &'a BTreeMap<String, String>,
    /// Its node; none where it is not placed.
    pub node: Option<Uuid>,
    /// Its addresses as that node last observed them.
    pub addresses: Vec<IpAddr>,
}

/// What the rule's node routes to the workloads of other nodes: the node, and each member of
/// the mesh with its routed pools. A node that is not a member routes to none of them.
#[derive(Debug, Clone, Copy)]
pub struct Routes<'a> {
    /// The rule's node.
    pub node: Uuid,
    /// Each member of the mesh, and its routed pools.
    pub members: &'a BTreeMap<Uuid, Vec<IpNet>>,
}

/// Why a workload a rule matches is named at no address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftOut {
    /// It is not placed.
    NotPlaced,
    /// Its node has not observed it at an address yet.
    NotObserved,
    /// It runs on a node the rule's node routes nothing of its to: a node outside the mesh, or
    /// the rule's node is outside it. It reaches the endpoint, if at all, translated, which no
    /// rule naming workloads matches.
    Unrouted,
}

impl LeftOut {
    /// Why, in words.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotPlaced => "it is not placed",
            Self::NotObserved => "its node has not observed it at an address yet",
            Self::Unrouted => {
                "it runs on a node this one routes nothing of its to, outside the mesh or not \
                 paired with it"
            }
        }
    }
}

/// One rule naming workloads, resolved: the addresses it names, each workload it matched, and
/// those it named at no address, with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Where the rule is.
    pub place: RulePlace,
    /// The addresses it names, in order, each once.
    pub addresses: Vec<IpAddr>,
    /// Each workload it matched.
    pub matched: Vec<Uuid>,
    /// Each it matched and named at no address, with why.
    pub left_out: Vec<(Uuid, LeftOut)>,
}

/// What a node is told of one rule naming workloads: where it is, and the addresses it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleAddresses {
    /// Where the rule is.
    pub place: RulePlace,
    /// The addresses it names; none where it names none now.
    pub addresses: Vec<IpAddr>,
}

impl From<&Resolved> for RuleAddresses {
    fn from(r: &Resolved) -> Self {
        Self { place: r.place.clone(), addresses: r.addresses.clone() }
    }
}

/// Whether `a` is a global IPv6 address, which no node gives another workload.
fn global(a: IpAddr) -> bool {
    matches!(a, IpAddr::V6(v6) if v6.segments()[0] & 0xe000 == 0x2000)
}

/// The addresses of `w` a rule on `routes`' node names, allowing or not.
fn named(allow: bool, w: &Located<'_>, routes: Routes<'_>) -> Result<Vec<IpAddr>, LeftOut> {
    let Some(node) = w.node else { return Err(LeftOut::NotPlaced) };
    if w.addresses.is_empty() {
        return Err(LeftOut::NotObserved);
    }
    if node == routes.node {
        return Ok(w.addresses.clone());
    }
    let pools = routes.members.get(&node).filter(|_| routes.members.contains_key(&routes.node));
    let reached = |a: &IpAddr| pools.is_some_and(|p| p.iter().any(|n| n.contains(a)));
    let out: Vec<IpAddr> =
        w.addresses.iter().copied().filter(|a| reached(a) || (!allow && global(*a))).collect();
    if out.is_empty() { Err(LeftOut::Unrouted) } else { Ok(out) }
}

/// Each rule of `body` naming workloads, resolved against the project's `workloads` for the
/// node `routes` names.
#[must_use]
pub fn resolve(body: &IntentBody, workloads: &[Located<'_>], routes: Routes<'_>) -> Vec<Resolved> {
    naming_workloads(body)
        .into_iter()
        .map(|n| {
            let mut resolved = Resolved {
                place: RulePlace { endpoint: n.place.0.map(str::to_owned), index: n.place.1 },
                addresses: Vec::new(),
                matched: Vec::new(),
                left_out: Vec::new(),
            };
            for w in workloads.iter().filter(|w| n.selector.matches(w.labels)) {
                resolved.matched.push(w.id);
                match named(n.allow, w, routes) {
                    Ok(addresses) => resolved.addresses.extend(addresses),
                    Err(why) => resolved.left_out.push((w.id, why)),
                }
            }
            resolved.addresses.sort_unstable();
            resolved.addresses.dedup();
            resolved
        })
        .collect()
}

#[cfg(test)]
mod tests;
