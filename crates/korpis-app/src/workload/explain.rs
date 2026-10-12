// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a workload is, and why, as one viewer may see it (§2.3 of docs/05-scheduling.md, §8 of
//! docs/08-identity.md). The store keeps the full derivation; this filters it per caller.

use super::read::WorkloadView;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::placement::{BoundBy, Placement, Unplaced};
use korpis_core::schedule::view::{ExplanationView, Sight, named, render};
use korpis_core::workload::spec::IsolationTier;
use korpis_core::workload::{Intent, Workload};
use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;
use uuid::Uuid;

/// A workload's placement, as the caller may see it.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacementView {
    /// The node, if the caller may read it.
    pub node: Option<Uuid>,
    /// The node's `region` label, which every reader of the workload sees.
    pub region: Option<String>,
    /// The tier it runs at.
    pub tier: IsolationTier,
    /// The approved intent it was bound for.
    pub intent: Uuid,
    /// When.
    pub bound_at: SystemTime,
    /// By whom.
    pub bound_by: BoundBy,
    /// Why, for a human.
    pub reason: String,
    /// Whether it is the authoritative one.
    pub authoritative: bool,
    /// The derivation, filtered.
    pub explanation: ExplanationView,
}

/// Why no node took a workload's target, as the caller may see it.
#[derive(Debug, Clone, PartialEq)]
pub struct UnplacedView {
    /// The target no node could take.
    pub intent: Uuid,
    /// When that was last tried.
    pub attempted_at: SystemTime,
    /// The derivation, filtered.
    pub explanation: ExplanationView,
}

/// What the store holds about where a workload is, before filtering.
#[derive(Debug, Clone, Default)]
pub(super) struct Found {
    placement: Option<(Placement, Option<String>, IsolationTier)>,
    unplaced: Option<Unplaced>,
}

impl Found {
    /// Whether the workload has an authoritative placement bound for `target`.
    pub(super) fn placed_for(&self, target: Option<Uuid>) -> bool {
        self.placement.as_ref().is_some_and(|(p, ..)| p.authoritative && Some(p.intent) == target)
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Reads a workload's placement, its node's region, its tier, and any failed attempt.
    pub(super) async fn found(
        &self,
        w: &Workload,
        target: Option<&Intent>,
    ) -> Result<Found, AppError> {
        let unplaced = self.store.unplaced(w.id).await?;
        let Some(p) = self.store.placement(w.id).await? else {
            return Ok(Found { placement: None, unplaced });
        };
        let region =
            self.store.node(p.node).await?.and_then(|n| n.labels.as_map().get("region").cloned());
        let tier = if let Some(t) = target.filter(|t| t.id == p.intent) {
            t.body.runtime.tier
        } else {
            let missing = || AppError::Internal(format!("placement intent {} missing", p.intent));
            self.store.intent(p.intent).await?.ok_or_else(missing)?.body.runtime.tier
        };
        Ok(Found { placement: Some((p, region, tier)), unplaced })
    }

    /// Fills each view's placement as the caller may see it: one evaluation of the caller's
    /// grants for every node the explanations name and every organization the workloads are in.
    pub(super) async fn render_placements(
        &self,
        caller: &Caller,
        views: &mut [WorkloadView],
        found: Vec<Found>,
    ) -> Result<(), AppError> {
        let mut nodes = BTreeSet::new();
        for f in &found {
            if let Some((p, ..)) = &f.placement {
                nodes.insert(p.node);
                nodes.extend(named(&p.explanation));
            }
            nodes.extend(f.unplaced.iter().flat_map(|u| named(&u.explanation)));
        }
        let nodes: Vec<Uuid> = nodes.into_iter().collect();
        let objects: Vec<ObjectRef> =
            nodes.iter().map(|n| ObjectRef::new(Kind::Node, *n)).collect();
        let readable = self.permitted_each(caller, "node.read", &objects).await?;
        let readable: BTreeSet<Uuid> =
            nodes.iter().zip(readable).filter_map(|(n, ok)| ok.then_some(*n)).collect();
        let orgs: Vec<Uuid> = views
            .iter()
            .map(|v| v.workload.organization)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let objects: Vec<ObjectRef> =
            orgs.iter().map(|o| ObjectRef::new(Kind::Organization, *o)).collect();
        let tenant: BTreeMap<Uuid, bool> = orgs
            .into_iter()
            .zip(self.permitted_each(caller, "workload.read", &objects).await?)
            .collect();
        for (v, f) in views.iter_mut().zip(found) {
            let sight = Sight {
                nodes: readable.clone(),
                tenant: tenant.get(&v.workload.organization).copied().unwrap_or(false),
            };
            v.placement = f.placement.map(|(p, region, tier)| PlacementView {
                node: Some(p.node).filter(|n| sight.nodes.contains(n)),
                region,
                tier,
                intent: p.intent,
                bound_at: p.bound_at,
                bound_by: p.bound_by,
                reason: p.reason,
                authoritative: p.authoritative,
                explanation: render(&p.explanation, &sight),
            });
            v.unplaced = f.unplaced.map(|u| UnplacedView {
                intent: u.intent,
                attempted_at: u.attempted_at,
                explanation: render(&u.explanation, &sight),
            });
        }
        Ok(())
    }
}
