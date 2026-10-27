// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placement policies and explanations (§2 of docs/05-scheduling.md).

use crate::scope::{selector_from_pb, selector_to_pb};
use crate::spec::health::{affinity_from_pb, affinity_to_pb};
use crate::{Result, invalid, wire_id};
use korpis_core::id::IdKind;
use korpis_core::placement::BoundBy;
use korpis_core::schedule::view::ExplanationView;
use korpis_core::schedule::{
    MAX_SPREAD, PlacementPolicy, Signal, SpreadConstraint, SpreadScope, Strategy,
};
use korpis_core::workload::spec::placement::MAX_AFFINITY;
use korpis_proto::korpis::v1 as pb;

enum_map!(strategy_to_pb, strategy_from_pb, Strategy, pb::PlacementStrategy, "a strategy", {
    Strategy::Packed => pb::PlacementStrategy::PLACEMENT_STRATEGY_PACKED,
    Strategy::Distributed => pb::PlacementStrategy::PLACEMENT_STRATEGY_DISTRIBUTED,
    Strategy::Balanced => pb::PlacementStrategy::PLACEMENT_STRATEGY_BALANCED,
});

enum_map!(bound_by_to_pb, bound_by_from_pb, BoundBy, pb::BoundBy, "a binder", {
    BoundBy::Scheduler => pb::BoundBy::BOUND_BY_SCHEDULER,
    BoundBy::Operator => pb::BoundBy::BOUND_BY_OPERATOR,
    BoundBy::Migration => pb::BoundBy::BOUND_BY_MIGRATION,
    BoundBy::Recovery => pb::BoundBy::BOUND_BY_RECOVERY,
    BoundBy::Failover => pb::BoundBy::BOUND_BY_FAILOVER,
});

fn spread_to_pb(s: &SpreadConstraint) -> pb::SpreadConstraint {
    let (scope, selector) = match &s.scope {
        SpreadScope::Project => (pb::SpreadScope::SPREAD_SCOPE_PROJECT, None),
        SpreadScope::Organization => (pb::SpreadScope::SPREAD_SCOPE_ORGANIZATION, None),
        SpreadScope::Selector(sel) => (pb::SpreadScope::SPREAD_SCOPE_SELECTOR, Some(sel)),
    };
    pb::SpreadConstraint {
        topology_key: s.topology_key.clone(),
        max_skew: s.max_skew,
        selector: selector.map(selector_to_pb).into(),
        scope: scope.into(),
        ..Default::default()
    }
}

fn spread_from_pb(field: &str, s: &pb::SpreadConstraint) -> Result<SpreadConstraint> {
    if s.topology_key.is_empty() {
        return Err(invalid(&format!("{field}.topology_key"), "required"));
    }
    let scope = match s.scope.as_known() {
        Some(pb::SpreadScope::SPREAD_SCOPE_PROJECT) => SpreadScope::Project,
        Some(pb::SpreadScope::SPREAD_SCOPE_ORGANIZATION) => SpreadScope::Organization,
        Some(pb::SpreadScope::SPREAD_SCOPE_SELECTOR) => SpreadScope::Selector(selector_from_pb(
            &format!("{field}.selector"),
            s.selector.as_option(),
        )?),
        _ => return Err(invalid(&format!("{field}.scope"), "project, organization, or selector")),
    };
    Ok(SpreadConstraint { topology_key: s.topology_key.clone(), max_skew: s.max_skew, scope })
}

/// The wire form of a placement policy.
#[must_use]
pub fn policy_to_pb(p: &PlacementPolicy) -> pb::PlacementPolicy {
    pb::PlacementPolicy {
        strategy: strategy_to_pb(p.strategy).into(),
        spread: p.spread.iter().map(spread_to_pb).collect(),
        affinity: p.affinity.iter().map(affinity_to_pb).collect(),
        weights: p.weights.iter().map(|(s, w)| (s.as_str().to_owned(), *w)).collect(),
        ..Default::default()
    }
}

/// A placement policy from the field `field`; an unspecified strategy is the default.
pub fn policy_from_pb(field: &str, p: &pb::PlacementPolicy) -> Result<PlacementPolicy> {
    if p.spread.len() > MAX_SPREAD {
        return Err(invalid(&format!("{field}.spread"), "at most 16 constraints"));
    }
    if p.affinity.len() > MAX_AFFINITY {
        return Err(invalid(&format!("{field}.affinity"), "at most 32 rules"));
    }
    let strategy = match p.strategy.as_known() {
        Some(pb::PlacementStrategy::PLACEMENT_STRATEGY_UNSPECIFIED) => Strategy::default(),
        _ => strategy_from_pb(&format!("{field}.strategy"), p.strategy)?,
    };
    let spread = p
        .spread
        .iter()
        .enumerate()
        .map(|(i, s)| spread_from_pb(&format!("{field}.spread[{i}]"), s))
        .collect::<Result<_>>()?;
    let affinity = p
        .affinity
        .iter()
        .enumerate()
        .map(|(i, a)| affinity_from_pb(&format!("{field}.affinity[{i}]"), a))
        .collect::<Result<_>>()?;
    let weights = p
        .weights
        .iter()
        .map(|(k, w)| {
            let at = format!("{field}.weights.{k}");
            let signal = Signal::parse(k).ok_or_else(|| invalid(&at, "not a signal"))?;
            if !w.is_finite() || *w < 0.0 {
                return Err(invalid(&at, "a finite, non-negative weight"));
            }
            Ok((signal, *w))
        })
        .collect::<Result<_>>()?;
    Ok(PlacementPolicy { strategy, spread, affinity, weights })
}

/// The wire form of an explanation as one caller may see it.
#[must_use]
pub fn explanation_to_pb(e: &ExplanationView) -> pb::Explanation {
    let node = |n| wire_id(IdKind::Node, n);
    pb::Explanation {
        candidates: e.candidates,
        filtered: e
            .filtered
            .iter()
            .map(|r| pb::FilterResult {
                node: node(r.node),
                check: r.check.as_str().to_owned(),
                detail: r.detail.clone(),
                ..Default::default()
            })
            .collect(),
        scored: e
            .scored
            .iter()
            .map(|s| pb::ScoreResult {
                node: node(s.node),
                signal: s.signal.as_str().to_owned(),
                value: s.value,
                weight: s.weight,
                ..Default::default()
            })
            .collect(),
        chosen: e.chosen.map(node).unwrap_or_default(),
        runner_up: e.runner_up.map(node),
        policy: e.policy.as_ref().map(policy_to_pb).into(),
        owner_policies: e
            .owners
            .iter()
            .flatten()
            .map(|(o, p)| pb::OwnerPolicy {
                organization: wire_id(IdKind::Organization, *o),
                policy: Some(policy_to_pb(p)).into(),
                ..Default::default()
            })
            .collect(),
        rejected: e
            .rejected
            .iter()
            .map(|(c, n)| pb::CheckCount {
                check: c.as_str().to_owned(),
                nodes: *n,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}
