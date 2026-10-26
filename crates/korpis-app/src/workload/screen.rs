// SPDX-License-Identifier: AGPL-3.0-or-later

//! The shortlist query (§2.1 of docs/05-scheduling.md), asked before a workload's target is bound
//! and outside the placement lock: the best scored nodes, as the store holds them now, are asked
//! at once whether their driver's `Prepare` accepts the target. What a node refuses is kept for
//! the target; what it does not answer passes it over for this bind alone. The bind, under the
//! lock, filters and scores again with both, so a node that changed meanwhile is judged as it is
//! then, and a node the shortlist did not reach is checked by `Prepare` after the bind.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, Feasible, PlacementWrite, RecipeSource, Store};
use korpis_core::schedule::{Candidate, Request, ranked};
use korpis_core::tenancy::quota::Bounds;
use korpis_core::workload::{Intent, Plan, PlanTarget, Workload};
use std::collections::BTreeMap;
use std::time::Duration;
use uuid::Uuid;

/// How many of the best scored nodes are asked.
pub(crate) const SHORTLIST: usize = 3;

/// How long the shortlist waits for every answer together.
pub(crate) const SCREEN_TIMEOUT: Duration = Duration::from_secs(5);

/// What the shortlist found for one target.
#[derive(Debug, Clone, Default)]
pub(crate) struct Screened {
    /// The target asked about; nothing found applies to any other.
    pub(crate) intent: Option<Uuid>,
    /// Refused by the node's driver, with its reason: kept for the target.
    pub(crate) refused: BTreeMap<Uuid, String>,
    /// Asked and not answered, with why: passed over for this bind alone.
    pub(crate) unanswered: BTreeMap<Uuid, String>,
}

impl Screened {
    /// What applies to target `intent`: everything when it is the one asked about, else nothing.
    pub(crate) fn for_target(
        &self,
        intent: Uuid,
    ) -> (&BTreeMap<Uuid, String>, &BTreeMap<Uuid, String>) {
        static NONE: BTreeMap<Uuid, String> = BTreeMap::new();
        if self.intent == Some(intent) { (&self.refused, &self.unanswered) } else { (&NONE, &NONE) }
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Asks the shortlist about `w`'s target `target`. Nothing is asked when no query is
    /// configured, when the workload is placed already (it follows its target on its node, and
    /// the check after the bind covers that node), or when the recipe is not kept.
    pub(crate) async fn screen(&self, w: &Workload, target: &Intent) -> Result<Screened, AppError> {
        let mut screened = Screened { intent: Some(target.id), ..Screened::default() };
        let Some(feasibility) = self.feasibility.as_ref() else { return Ok(screened) };
        if self.store.placement(w.id).await?.is_some() {
            return Ok(screened);
        }
        let Some(recipe) = self.store.cached_recipe(&target.body.recipe.digest).await? else {
            return Ok(screened);
        };
        let shortlist = self.shortlist(w, target).await?;
        if shortlist.is_empty() {
            return Ok(screened);
        }
        let asked = feasibility.ask(&shortlist, &target.body, &recipe.document, SCREEN_TIMEOUT);
        for (node, answer) in asked.await {
            match answer {
                Feasible::Accepted => {}
                Feasible::Refused(why) => {
                    screened.refused.insert(node, why);
                }
                Feasible::Unanswered => {
                    let why = format!("it did not answer within {} s", SCREEN_TIMEOUT.as_secs());
                    screened.unanswered.insert(node, why);
                }
            }
        }
        Ok(screened)
    }

    /// As [`Self::screen`], for whatever target workload `id` has now.
    pub(crate) async fn screen_current(&self, id: Uuid) -> Result<Screened, AppError> {
        let Some(w) = self.store.workload(id).await? else { return Ok(Screened::default()) };
        let Some(t) = w.target else { return Ok(Screened::default()) };
        match self.store.intent(t).await? {
            Some(target) => self.screen(&w, &target).await,
            None => Ok(Screened::default()),
        }
    }

    /// As [`Self::screen`], for the intent `plan` approves; nothing for any other plan.
    pub(crate) async fn screen_plan(&self, plan: &Plan) -> Result<Screened, AppError> {
        let PlanTarget::Intent(i) = plan.target else { return Ok(Screened::default()) };
        let w = self.store.workload(plan.workload).await?;
        match (w, self.store.intent(i).await?) {
            (Some(w), Some(target)) => self.screen(&w, &target).await,
            _ => Ok(Screened::default()),
        }
    }

    /// The best scored nodes for `target`, as the store holds them now: read in a transaction
    /// that takes no lock and is never committed.
    async fn shortlist(&self, w: &Workload, target: &Intent) -> Result<Vec<Uuid>, AppError> {
        let owners = self.owners(w.organization).await?;
        let volumes = self.existing_nodes(&target.body).await?;
        let (needed, mesh) = (self.needed(w, &target.body).await?, self.judged().await?.pairs());
        let mut tx = self.store.begin().await?;
        let nodes = tx.serving_nodes(&owners).await?;
        let ids: Vec<Uuid> = nodes.iter().map(|n| n.id).collect();
        let allocations = tx.allocations(&ids).await?;
        let held = tx.device_holds(&ids, w.id).await?;
        let held_cores = tx.core_holds(&ids, w.id).await?;
        let peers = tx.peers(w.organization, w.id).await?;
        let refused = tx.refusals(w.id, target.id).await?;
        let learned = self.learned(&mut tx, &ids, target).await?;
        let dedicated =
            self.dedicated_nodes(&mut tx, (w.id, &target.body), &owners, &nodes).await?;
        drop(tx);
        let empty = Bounds::default();
        let candidates: Vec<Candidate<'_>> = nodes
            .iter()
            .map(|n| Candidate { node: n, allocated: allocations.get(&n.id).unwrap_or(&empty) })
            .collect();
        let req = Request {
            workload: w.id,
            project: w.project,
            labels: w.labels.as_map(),
            owners: &owners,
            body: &target.body,
            current: None,
            edge_tier: super::place::EDGE_TIER,
            volumes: &volumes,
            refused: &refused,
            content: &learned.content,
            churn: &learned.churn,
            dependencies: &needed,
            mesh: &mesh,
            // Quota is checked at the bind, under the placement lock that holds its totals; the
            // shortlist only asks drivers.
            quota: &[],
            devices: &held,
            cores: &held_cores,
            dedicated: &dedicated,
        };
        let (_, ranking) = ranked(&req, &candidates, &peers, &self.policies(&owners).await?);
        Ok(ranking.into_iter().take(SHORTLIST).collect())
    }
}
