// SPDX-License-Identifier: AGPL-3.0-or-later

//! An explanation as one viewer may see it (§8 of docs/08-identity.md). The stored explanation is
//! the full derivation; what a subject sees is filtered by their grants on read, so there is
//! never a second version of the truth.
//!
//! ```text
//! a node the viewer may read        its rejections, its scores, whether it was chosen
//! the tenant (reads the workload    how many candidates, and how many failed each check
//!   across its organization)
//! anyone who may read the workload  which checks failed, with no counts and no nodes
//! the policies                      only to a viewer who may read every node it names
//! ```

use super::{Check, Explanation, PlacementPolicy, Rejection, Score};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// What a viewer's grants let them see.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sight {
    /// The nodes they may read.
    pub nodes: BTreeSet<Uuid>,
    /// Whether they may read the workload's whole organization: the tenant, not one workload's
    /// owner.
    pub tenant: bool,
}

/// An explanation, filtered for one viewer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExplanationView {
    /// Nodes considered; tenants only.
    pub candidates: Option<u32>,
    /// Per failing check, how many nodes failed it; the count for tenants only.
    pub rejected: Vec<(Check, Option<u32>)>,
    /// The rejections of nodes the viewer may read.
    pub filtered: Vec<Rejection>,
    /// The scores of nodes the viewer may read.
    pub scored: Vec<Score>,
    /// The node chosen, if the viewer may read it.
    pub chosen: Option<Uuid>,
    /// The next best, if the viewer may read it.
    pub runner_up: Option<Uuid>,
    /// The workload's policy, if the viewer may read every node the explanation names.
    pub policy: Option<PlacementPolicy>,
    /// The policy each owner's nodes were scored by, under the same condition.
    pub owners: Option<BTreeMap<Uuid, PlacementPolicy>>,
}

/// Every node an explanation names.
#[must_use]
pub fn named(e: &Explanation) -> BTreeSet<Uuid> {
    let mut out: BTreeSet<Uuid> = e.filtered.iter().map(|r| r.node).collect();
    out.extend(e.scored.iter().map(|s| s.node));
    out.extend(e.chosen);
    out.extend(e.runner_up);
    out
}

/// `e` as the viewer with `sight` may see it.
#[must_use]
pub fn render(e: &Explanation, sight: &Sight) -> ExplanationView {
    let all = named(e).iter().all(|n| sight.nodes.contains(n));
    let sees = |n: &Uuid| sight.nodes.contains(n);
    let counts = sight.tenant;
    let mut per: BTreeMap<Check, BTreeSet<Uuid>> = BTreeMap::new();
    for r in &e.filtered {
        per.entry(r.check).or_default().insert(r.node);
    }
    let rejected = per
        .into_iter()
        .map(|(c, nodes)| (c, counts.then(|| u32::try_from(nodes.len()).unwrap_or(u32::MAX))))
        .collect();
    ExplanationView {
        candidates: counts.then_some(e.candidates),
        rejected,
        filtered: e.filtered.iter().filter(|r| sees(&r.node)).cloned().collect(),
        scored: e.scored.iter().filter(|s| sees(&s.node)).copied().collect(),
        chosen: e.chosen.filter(sees),
        runner_up: e.runner_up.filter(sees),
        policy: all.then(|| e.policy.clone()),
        owners: all.then(|| e.owners.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::Signal;

    fn explanation() -> Explanation {
        let (a, b, c) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        let reject = |node, check| Rejection { node, check, detail: "x".into() };
        let score = |node| Score { node, signal: Signal::Utilization, value: 0.5, weight: 1.0 };
        Explanation {
            candidates: 3,
            filtered: vec![reject(c, Check::Tier), reject(c, Check::Capacity)],
            scored: vec![score(a), score(b)],
            chosen: Some(a),
            runner_up: Some(b),
            policy: PlacementPolicy::default(),
            owners: BTreeMap::new(),
        }
    }

    #[test]
    fn the_operator_sees_everything() {
        let e = explanation();
        let v = render(&e, &Sight { nodes: named(&e), tenant: true });
        assert_eq!((v.filtered.len(), v.scored.len(), v.chosen), (2, 2, e.chosen));
        assert!(v.policy.is_some());
    }

    #[test]
    fn a_tenant_sees_counts_and_no_node() {
        let v = render(&explanation(), &Sight { nodes: BTreeSet::new(), tenant: true });
        assert_eq!(v.candidates, Some(3));
        assert_eq!(v.rejected, [(Check::Tier, Some(1)), (Check::Capacity, Some(1))]);
        assert!(v.filtered.is_empty() && v.scored.is_empty() && v.chosen.is_none());
        assert!(v.policy.is_none());
    }

    #[test]
    fn a_workload_owner_sees_which_checks_and_nothing_countable() {
        let v = render(&explanation(), &Sight::default());
        assert_eq!(v.candidates, None);
        assert_eq!(v.rejected, [(Check::Tier, None), (Check::Capacity, None)]);
    }

    #[test]
    fn a_reader_of_some_nodes_sees_those() {
        let only_c = Sight { nodes: [Uuid::from_u128(3)].into(), tenant: false };
        let v = render(&explanation(), &only_c);
        assert_eq!((v.filtered.len(), v.scored.len(), v.chosen), (2, 0, None));
        assert!(v.policy.is_none(), "it names nodes this viewer may not read");
    }
}
