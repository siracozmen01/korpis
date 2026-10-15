// SPDX-License-Identifier: AGPL-3.0-or-later

//! Computing a node's assignment set (§4.8 of docs/02-architecture.md). One assignment per
//! workload placed on the node: its approved intent and power, the steps of its applying plan
//! the node executes, whether it is being retired, and the recipe document it runs. An
//! assignment whose content has not changed keeps its generation and signature; one that has is
//! sealed again one generation higher; one that left the set is kept, inactive, so its
//! generation never goes back.
//!
//! A workload that no longer runs on the node may still have a plan there: deleting a volume it
//! left behind (§7 of docs/06-storage.md). It is sent a residual assignment, which carries that
//! plan and nothing the node acts on besides; it leaves the set once the plan is done.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    AssignmentWrite, Clock, RecipeSource, SealError, Sealed, Store, StoredAssignment, Tx,
    WorkloadWrite,
};
use korpis_core::workload::Plan;
use korpis_core::workload::assignment::{Assignment, IdRange, NodePlan, retires, set_digest};
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

/// The id range `workload` keeps on `node`, allocated on its first assignment there.
async fn ids<T: Tx>(tx: &mut T, node: Uuid, workload: Uuid) -> Result<IdRange, AppError> {
    let slot = tx.id_slot(node, workload, IdRange::slots()).await?;
    slot.and_then(IdRange::of_slot).ok_or_else(|| {
        AppError::conflict(
            "node.ids_exhausted",
            "every user-namespace id range on the node is held",
        )
    })
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Computes `node`'s set, seals what changed, and returns the set to send.
    pub async fn sync_assignments(&self, node: Uuid) -> Result<AssignmentSet, AppError> {
        let sealer = self
            .assignment_sealer
            .as_ref()
            .ok_or_else(|| AppError::Internal("no assignment sealer is configured".into()))?;
        let desired = self.all_desired(node).await?;
        let mut tx = self.store.begin().await?;
        tx.lock_assignments(node).await?;
        let stored: BTreeMap<Uuid, StoredAssignment> =
            tx.stored_assignments(node).await?.into_iter().map(|s| (s.workload, s)).collect();
        let mut out = Vec::with_capacity(desired.len());
        for (mut assignment, plan) in desired {
            if !assignment.residual {
                assignment.ids = ids(&mut tx, node, assignment.workload).await?;
            }
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

    /// What `node` is to be told: an assignment for each workload placed on it, and a residual
    /// one for each that left a plan there.
    async fn all_desired(&self, node: Uuid) -> Result<Vec<(Assignment, Option<Plan>)>, AppError> {
        let mut desired = Vec::new();
        for workload in self.store.placed_on(node).await? {
            desired.extend(self.desired(node, workload).await?);
        }
        for workload in self.store.residual_on(node).await? {
            desired.extend(self.residual(node, workload).await?);
        }
        Ok(desired)
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
        let missing = |what: &str| AppError::Internal(format!("{what} of workload {workload}"));
        let plan = active_plan(self.store.open_plans(workload).await?, self.clock.now());
        let Some((target, power)) = self.told(&w, plan.as_ref()).await? else { return Ok(None) };
        let manifest = self
            .store
            .cached_recipe(&target.body.recipe.digest)
            .await?
            .ok_or_else(|| missing("recipe document"))?;
        // No storage class a Phase 0 node offers is attachable from a second node, so a derived
        // answer keeps running (§4.5 of docs/02-architecture.md).
        let on_lease_expiry = target.body.continuity.resolved(false);
        let lease_epoch = self.store.lease_epoch(workload, node).await?.unwrap_or(0);
        let volume_ids = self.existing_ids(&target.body).await?;
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
                ids: IdRange { base: 0, count: 0 },
                residual: false,
                volume_ids,
            },
            plan,
        )))
    }

    /// The residual assignment of `workload` on `node`: the plan deleting a volume it left there,
    /// with the last target and power it had for the record. No ids, no lease, no recipe it
    /// needs; `None` once nothing is left to do there.
    async fn residual(
        &self,
        node: Uuid,
        workload: Uuid,
    ) -> Result<Option<(Assignment, Option<Plan>)>, AppError> {
        let Some(w) = self.store.workload(workload).await? else { return Ok(None) };
        let missing = |what: &str| AppError::Internal(format!("{what} of workload {workload}"));
        let id = w.target.ok_or_else(|| missing("target"))?;
        let target = self.store.intent(id).await?.ok_or_else(|| missing("target"))?;
        let power = self
            .store
            .power_at(workload, w.power_target_version)
            .await?
            .ok_or_else(|| missing("power target"))?;
        let mut here = Vec::new();
        for p in self.store.open_plans(workload).await? {
            if let Some(v) = p.target.volume()
                && self.store.volume(v).await?.is_some_and(|v| v.node == node)
            {
                here.push(p);
            }
        }
        let Some(plan) = active_plan(here, self.clock.now()) else { return Ok(None) };
        let on_lease_expiry = target.body.continuity.resolved(false);
        Ok(Some((
            Assignment {
                workload,
                project: w.project,
                organization: w.organization,
                generation: 0,
                target,
                power,
                on_lease_expiry,
                plan: NodePlan::of(&plan),
                retire: None,
                recipe_manifest: Vec::new(),
                node,
                lease_epoch: 0,
                ids: IdRange { base: 0, count: 0 },
                residual: true,
                volume_ids: BTreeMap::new(),
            },
            Some(plan),
        )))
    }
}
