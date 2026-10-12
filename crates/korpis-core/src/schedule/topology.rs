// SPDX-License-Identifier: AGPL-3.0-or-later

//! Topology domains: what "together" and "apart" mean for affinity and spread.
//!
//! A node's domain under a key is the value of its label of that name. The key `host` means the
//! node itself unless the node carries a `host` label. A node without the label is in no known
//! domain: nothing can show that a required rule holds there, so it is rejected, and a
//! preference scores it zero.

use super::Peer;
use crate::labels::LabelSelector;
use crate::node::Node;
use std::collections::BTreeMap;
use uuid::Uuid;

/// Every node a peer or a candidate may be on, by identifier.
pub(super) type Fleet<'a> = BTreeMap<Uuid, &'a Node>;

/// The node's domain under `key`.
pub(super) fn domain(node: &Node, key: &str) -> Option<String> {
    match node.labels.as_map().get(key) {
        Some(v) => Some(v.clone()),
        None if key == "host" => Some(node.id.to_string()),
        None => None,
    }
}

/// The peers other than `except` that `selector` matches, with the domain of the node each is
/// on under `key` (`None` for a node in no domain or not in the fleet).
pub(super) fn matching<'p>(
    fleet: &Fleet<'_>,
    peers: &'p [Peer],
    key: &str,
    selector: &LabelSelector,
    except: Uuid,
) -> Vec<(&'p Peer, Option<String>)> {
    peers
        .iter()
        .filter(|p| p.workload != except && selector.matches(&p.labels))
        .map(|p| (p, fleet.get(&p.node).and_then(|n| domain(n, key))))
        .collect()
}

/// How many matching peers each domain holds, over the domains of `nodes` (so an empty domain
/// counts as zero) and the domains the peers are in.
pub(super) fn counts(
    nodes: &[&Node],
    matched: &[(&Peer, Option<String>)],
    key: &str,
) -> BTreeMap<String, u64> {
    let mut out: BTreeMap<String, u64> =
        nodes.iter().filter_map(|n| domain(n, key)).map(|d| (d, 0)).collect();
    for d in matched.iter().filter_map(|(_, d)| d.clone()) {
        *out.entry(d).or_insert(0) += 1;
    }
    out
}
