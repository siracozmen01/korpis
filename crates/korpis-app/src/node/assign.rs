// SPDX-License-Identifier: AGPL-3.0-or-later

//! Computing a node's assignment set (§4.8 of docs/02-architecture.md). One assignment per
//! workload placed on the node: its approved intent and power, the steps of its applying plan
//! the node executes, whether it is being retired, and the recipe document it runs. An
//! assignment whose content has not changed keeps its generation and signature; one that has is
//! sealed again one generation higher; one that left the set is kept, inactive, so its
//! generation never goes back.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    AssignmentWrite, Clock, RecipeSource, SealError, Sealed, Store, StoredAssignment, Tx,
    WorkloadWrite,
};
use korpis_core::workload::Plan;
use korpis_core::workload::assignment::{Assignment, NodePlan, retires, set_digest};
use korpis_core::workload::plan::PlanStatus;
use std::collections::BTreeMap;
use std::time::SystemTime;
use uuid::Uuid;

/// A node's set, as it is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignmentSet {
    /// Each assignment, sorted by workload.
    pub assignments: Vec<StoredAssignment>,
    /// Over the set (§9 of docs/10-api.md).
    pub digest: String,
}

fn seal_error(SealError(why): SealError) -> AppError {
    AppError::Internal(format!("sealing an assignment: {why}"))
}

/// The plan a node is to be executing for a workload: the applying one, or else the earliest
/// approved or queued whose window has opened, which becomes applying when it is first assigned.
/// A queued plan waited for the one ahead of it; once that one is done, it is next.
fn active_plan(open: Vec<Plan>, now: SystemTime) -> Option<Plan> {
    if let Some(applying) = open.iter().find(|p| p.status == PlanStatus::Applying) {
        return Some(applying.clone());
    }
    let mut ready: Vec<Plan> = open
        .into_iter()
        .filter(|p| matches!(p.status, PlanStatus::Approved | PlanStatus::Queued))
        .filter(|p| p.apply_after.is_none_or(|t| t <= now))
        .collect();
    ready.sort_by_key(|p| (p.approved_at, p.id));
    ready.into_iter().next()
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Computes `node`'s set, seals what changed, and returns the set to send.
    pub async fn sync_assignments(&self, node: Uuid) -> Result<AssignmentSet, AppError> {
        let sealer = self
            .assignment_sealer
            .as_ref()
            .ok_or_else(|| AppError::Internal("no assignment sealer is configured".into()))?;
        let mut desired = Vec::new();
        for workload in self.store.placed_on(node).await? {
            if let Some(d) = self.desired(node, workload).await? {
                desired.push(d);
            }
        }
        let mut tx = self.store.begin().await?;
        tx.lock_assignments(node).await?;
        let stored: BTreeMap<Uuid, StoredAssignment> =
            tx.stored_assignments(node).await?.into_iter().map(|s| (s.workload, s)).collect();
        let mut out = Vec::with_capacity(desired.len());
        for (mut assignment, plan) in desired {
            let digest = sealer.content_digest(&assignment).map_err(seal_error)?;
            let kept = stored.get(&assignment.workload);
            let current = match kept {
                Some(s) if s.active && s.content_digest == digest => s.clone(),
                _ => {
                    assignment.generation = kept.map_or(1, |s| s.generation + 1);
                    let signed: Sealed = sealer.seal(&assignment).map_err(seal_error)?;
                    let fresh = StoredAssignment {
                        workload: assignment.workload,
                        generation: assignment.generation,
                        content_digest: digest,
                        sealed: signed,
                        active: true,
                    };
                    tx.put_assignment(node, &fresh).await?;
                    fresh
                }
            };
            if let Some(plan) = plan {
                self.begin_applying(&mut tx, plan).await?;
            }
            out.push(current);
        }
        for gone in stored.values().filter(|s| s.active) {
            if !out.iter().any(|a| a.workload == gone.workload) {
                tx.deactivate_assignment(node, gone.workload).await?;
            }
        }
        tx.commit().await?;
        let entries: Vec<(Uuid, u64)> = out.iter().map(|a| (a.workload, a.generation)).collect();
        Ok(AssignmentSet { digest: set_digest(&entries), assignments: out })
    }

    /// The nodes whose sets a change to `workload` may alter.
    pub async fn nodes_concerned(&self, workload: Uuid) -> Result<Vec<Uuid>, AppError> {
        Ok(self.store.nodes_of(workload).await?)
    }

    /// Records that a node holds the set with `digest`.
    pub async fn acknowledge_set(&self, node: Uuid, digest: &str) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.record_ack(node, digest, self.clock.now()).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Marks an approved or queued plan applying, unless another of its workload's already is.
    async fn begin_applying(&self, tx: &mut S::Tx, plan: Plan) -> Result<(), AppError> {
        let from = plan.status;
        let waiting = matches!(from, PlanStatus::Approved | PlanStatus::Queued);
        if !waiting || tx.plan_applying(plan.workload).await? {
            return Ok(());
        }
        let applying = Plan { status: PlanStatus::Applying, ..plan };
        tx.update_plan(&applying, from).await?;
        Ok(())
    }

    /// What `node` is to be told about `workload`, with the plan that becomes applying by being
    /// told; `None` if nothing approved reaches a node yet.
    async fn desired(
        &self,
        node: Uuid,
        workload: Uuid,
    ) -> Result<Option<(Assignment, Option<Plan>)>, AppError> {
        let Some(w) = self.store.workload(workload).await? else { return Ok(None) };
        let Some(target) = w.target else { return Ok(None) };
        let missing = |what: &str| AppError::Internal(format!("{what} of workload {workload}"));
        let target = self.store.intent(target).await?.ok_or_else(|| missing("target"))?;
        let power = self
            .store
            .power_at(workload, w.power_target_version)
            .await?
            .ok_or_else(|| missing("power target"))?;
        let manifest = self
            .store
            .cached_recipe(&target.body.recipe.digest)
            .await?
            .ok_or_else(|| missing("recipe document"))?;
        let plan = active_plan(self.store.open_plans(workload).await?, self.clock.now());
        // No storage class a Phase 0 node offers is attachable from a second node, so a derived
        // answer keeps running (§4.5 of docs/02-architecture.md).
        let on_lease_expiry = target.body.continuity.resolved(false);
        let lease_epoch = self.store.lease_epoch(workload, node).await?.unwrap_or(0);
        Ok(Some((
            Assignment {
                workload,
                project: w.project,
                organization: w.organization,
                generation: 0,
                target,
                power,
                on_lease_expiry,
                plan: plan.as_ref().and_then(NodePlan::of),
                retire: plan.as_ref().filter(|p| retires(p)).map(|p| p.id),
                recipe_manifest: manifest.document,
                node,
                lease_epoch,
            },
            plan,
        )))
    }
}
