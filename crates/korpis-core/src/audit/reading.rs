// SPDX-License-Identifier: AGPL-3.0-or-later

//! Who reads an effect, and what of it (§4 of docs/15-observability.md): the read an effect is
//! shown under, and the nodes it names, which a reader sees only where they may read each of
//! them (§8 of docs/08-identity.md).

use super::Effect;
use crate::id::{Id, IdKind};
use crate::object::{Kind, ObjectRef};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use uuid::Uuid;

/// The families of action that read their effects under a read of their own, beside the read of
/// what the effect is about (§4 of docs/15-observability.md).
const FAMILY_READS: [(&str, &str); 5] = [
    ("grant", "grant.read"),
    ("plan", "plan.read"),
    ("extension", "extension.read"),
    ("backup", "backup.read"),
    ("event", "event.read"),
];

/// What a caller must hold over `object` to read an effect of `action` about it: the read of
/// the action's family where the family has one, and otherwise the read of the object's kind
/// (§4 of docs/15-observability.md).
#[must_use]
pub fn reader(action: &str, object: ObjectRef) -> &'static str {
    let family = action.split('.').next().unwrap_or_default();
    if let Some((_, read)) = FAMILY_READS.iter().find(|(f, _)| *f == family) {
        return read;
    }
    match object.kind {
        Kind::Organization => "org.read",
        Kind::Project => "project.read",
        Kind::Workload | Kind::Stream => "workload.read",
        Kind::Volume => "volume.read",
        Kind::Endpoint => "endpoint.read",
        Kind::Node => "node.read",
        Kind::Repository => "repository.read",
        Kind::Domain => "domain.read",
        Kind::Certificate => "certificate.read",
        Kind::Portal => "portal.read",
    }
}

/// Whether a detail's key says something of a node: `node`, `nodes`, `node_…`, or `…_node`.
fn node_key(key: &str) -> bool {
    key == "node" || key == "nodes" || key.starts_with("node_") || key.ends_with("_node")
}

/// The node `v` identifies, as a bare identifier or a wire one.
fn node_id(v: &Value) -> Option<Uuid> {
    let s = v.as_str()?;
    Uuid::parse_str(s).ok().or_else(|| Id::parse(IdKind::Node, s).ok().map(|i| i.uuid()))
}

/// Whether `v` is a node's identifier or a list of nothing else.
fn identifies(v: &Value) -> bool {
    match v {
        Value::Array(items) => items.iter().all(|i| node_id(i).is_some()),
        v => node_id(v).is_some(),
    }
}

fn details(e: &Effect) -> impl Iterator<Item = &Map<String, Value>> {
    [&e.before, &e.after, &e.error].into_iter().flatten()
}

/// The nodes `e` names by identifier: its node, its journal origin's, its object where that is
/// a node, and each identifier a node key holds among its details, alone or in a list.
#[must_use]
pub fn nodes_named(e: &Effect) -> BTreeSet<Uuid> {
    let mut named: BTreeSet<Uuid> = e.node.into_iter().chain(e.origin.map(|o| o.node)).collect();
    named.extend(e.object.filter(|o| o.kind == Kind::Node).map(|o| o.id));
    for (_, v) in details(e).flat_map(|d| d.iter()).filter(|(k, _)| node_key(k)) {
        match v {
            Value::Array(items) => named.extend(items.iter().filter_map(node_id)),
            v => named.extend(node_id(v)),
        }
    }
    named
}

/// Where a caller must hold `node.read` to be shown every node `e` names: over each node it
/// names by identifier, and, where it says something of a node it does not identify (an
/// enrollment's node name, a node's key), over what it is about, or else over `root`. Nothing,
/// for an effect that says nothing of a node.
#[must_use]
pub fn node_reads(e: &Effect, root: ObjectRef) -> Vec<ObjectRef> {
    let named = nodes_named(e);
    let unnamed = details(e).flat_map(|d| d.iter()).any(|(k, v)| node_key(k) && !identifies(v));
    let mut reads: Vec<ObjectRef> =
        named.into_iter().map(|n| ObjectRef::new(Kind::Node, n)).collect();
    if unnamed {
        reads.push(e.concerns().filter(|o| o.kind != Kind::Node).unwrap_or(root));
    }
    reads
}

/// `e` without anything that says something of a node, for a reader who may not read every
/// node it names (§8 of docs/08-identity.md): its node, its journal position, its object where
/// that is a node, and every detail under a node key.
pub fn without_nodes(e: &mut Effect) {
    e.node = None;
    e.origin = None;
    if e.object.is_some_and(|o| o.kind == Kind::Node) {
        e.object = None;
    }
    for details in [&mut e.before, &mut e.after, &mut e.error].into_iter().flatten() {
        details.retain(|k, _| !node_key(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{JournalOrigin, Outcome, details as pairs};
    use crate::principal::Principal;

    fn effect(action: &str) -> Effect {
        Effect::new(Principal::Anonymous, action, Outcome::Succeeded)
    }

    #[test]
    fn an_effect_is_read_under_its_familys_read_or_else_its_objects() {
        let w = ObjectRef::new(Kind::Workload, Uuid::now_v7());
        let org = ObjectRef::new(Kind::Organization, Uuid::now_v7());
        assert_eq!(reader("grant.issued", org), "grant.read");
        assert_eq!(reader("plan.approve", w), "plan.read");
        assert_eq!(reader("workload.restart", w), "workload.read");
        assert_eq!(reader("org.theme.set", org), "org.read");
        assert_eq!(reader("lease.issue", w), "workload.read", "lease has no read of its own");
    }

    #[test]
    fn every_node_an_effect_names_is_one_to_be_read() {
        let (now, before, origin) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let mut e = effect("lease.issue").after(pairs([
            ("previous_node", before.to_string().into()),
            ("nodes", vec![Id::from_uuid(IdKind::Node, now).to_string()].into()),
            ("epoch", 2.into()),
        ]));
        (e.node, e.origin) = (Some(now), Some(JournalOrigin { node: origin, seq: 1 }));
        let root = ObjectRef::new(Kind::Organization, Uuid::now_v7());
        assert_eq!(nodes_named(&e), BTreeSet::from([now, before, origin]));
        assert_eq!(node_reads(&e, root).len(), 3, "the previous holder is asked about too");

        let about = ObjectRef::new(Kind::Node, Uuid::now_v7());
        let granted = effect("grant.issued").about(about);
        assert_eq!(node_reads(&granted, root), [about], "a grant about a node names it");
        assert!(node_reads(&effect("org.theme.set"), root).is_empty());
    }

    #[test]
    fn a_node_said_but_not_identified_is_read_over_what_the_effect_is_about() {
        let home = ObjectRef::new(Kind::Organization, Uuid::now_v7());
        let root = ObjectRef::new(Kind::Organization, Uuid::now_v7());
        let enrolled = effect("node.enrollment.create")
            .about(home)
            .after(pairs([("node_name", "fra-1".into()), ("token", "t".into())]));
        assert_eq!(node_reads(&enrolled, root), [home]);
        let keyed = effect("node.enroll").after(pairs([("node_public_key", "k".into())]));
        assert_eq!(node_reads(&keyed, root), [root], "about nothing, so read at the root");
    }

    #[test]
    fn a_reader_who_may_not_read_its_nodes_reads_no_node_of_it() {
        let node = ObjectRef::new(Kind::Node, Uuid::now_v7());
        let mut e = effect("grant.issued").about(node).after(pairs([
            ("node", "n".into()),
            ("previous_node", "p".into()),
            ("node_name", "fra-1".into()),
            ("nodes", Value::Array(Vec::new())),
            ("epoch", 2.into()),
        ]));
        e.node = Some(node.id);
        e.origin = Some(JournalOrigin { node: node.id, seq: 1 });
        without_nodes(&mut e);
        assert_eq!((e.node, e.origin, e.object), (None, None, None));
        assert_eq!(e.after.unwrap().keys().collect::<Vec<_>>(), ["epoch"]);
    }
}
