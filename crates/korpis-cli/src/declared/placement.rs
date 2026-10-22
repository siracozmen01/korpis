// SPDX-License-Identifier: AGPL-3.0-or-later

//! A declared file's `placement` (§3 of docs/13-surface-cli.md): the nodes a workload may run on
//! and how it stands towards other workloads, as the intent body's placement (§2 of
//! docs/05-scheduling.md) says them, its keys the API's in snake case and its operators without
//! their prefix.

use super::named;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A label selector: every pair of `match_labels` present with that value, and every expression
/// holding.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    #[serde(default)]
    match_labels: BTreeMap<String, String>,
    #[serde(default)]
    match_expressions: Vec<Expression>,
}

/// One expression of a selector: `in`, `not_in`, `exists`, or `does_not_exist`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expression {
    key: String,
    operator: String,
    #[serde(default)]
    values: Vec<String>,
}

/// A taint a workload tolerates.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Toleration {
    key: String,
    #[serde(default)]
    value: String,
}

/// An affinity rule: to be with, or with `anti` away from, the workloads `workloads` selects in
/// the domain `topology_key` names (`host` for a node), required or preferred.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Affinity {
    #[serde(default)]
    anti: bool,
    #[serde(default)]
    required: bool,
    topology_key: String,
    workloads: Selector,
}

/// Where a workload may be placed.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    node_selector: Option<Selector>,
    #[serde(default)]
    tolerations: Vec<Toleration>,
    #[serde(default)]
    affinity: Vec<Affinity>,
    preferred_region: Option<String>,
}

fn selector(s: &Selector) -> Value {
    let expressions: Vec<Value> = s
        .match_expressions
        .iter()
        .map(|e| {
            json!({
                "key": e.key,
                "operator": named("LABEL_OPERATOR", Some(&e.operator)),
                "values": e.values,
            })
        })
        .collect();
    json!({ "matchLabels": s.match_labels, "matchExpressions": expressions })
}

/// The placement as the API's JSON carries it.
pub fn placement(p: &Placement) -> Value {
    let tolerations: Vec<Value> =
        p.tolerations.iter().map(|t| json!({ "key": t.key, "value": t.value })).collect();
    let affinity: Vec<Value> = p
        .affinity
        .iter()
        .map(|a| {
            json!({
                "anti": a.anti,
                "required": a.required,
                "workloads": selector(&a.workloads),
                "topologyKey": a.topology_key,
            })
        })
        .collect();
    let mut placed = json!({
        "tolerations": tolerations,
        "affinity": affinity,
        "preferredRegion": p.preferred_region.clone().unwrap_or_default(),
    });
    if let (Some(s), Some(o)) = (&p.node_selector, placed.as_object_mut()) {
        o.insert("nodeSelector".into(), selector(s));
    }
    placed
}
