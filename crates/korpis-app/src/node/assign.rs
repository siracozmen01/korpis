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

use super::endpoints;
use crate::App;
use crate::error::AppError;
use crate::ports::{
    AssignmentWrite, CertificateWrite as _, Clock, RecipeSource, RunWrite as _, SealError, Sealed,
    Store, StoredAssignment, Tx, WorkloadWrite,
};
use korpis_core::authz::record::{GrantRecord, in_force};
use korpis_core::principal::Principal;
use korpis_core::workload::assignment::{Assignment, IdRange, NodePlan, retires, set_digest};
use korpis_core::workload::plan::PlanStatus;
use korpis_core::workload::run::named;
use korpis_core::workload::{Plan, PowerState};
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

/// The digest of what `assignment` has its node run: all of it but its plan, which is measured by
/// its own progress, its ACME challenges, which the proxy answers for a minute and which change
/// nothing the workload runs (§4.3 of docs/02-architecture.md), and its labels, which the node
/// does not use. Its name is kept, since a name always has a value; nothing renames a workload.
fn runs_digest(
    sealer: &dyn crate::ports::AssignmentSealer,
    assignment: &Assignment,
) -> Result<Vec<u8>, AppError> {
    let runs = Assignment {
        plan: None,
        challenges: Vec::new(),
        alpn_challenges: Vec::new(),
        runs: Vec::new(),
        kept_runs: Vec::new(),
        labels: korpis_core::labels::Labels::default(),
        ..assignment.clone()
    };
    sealer.content_digest(&runs).map_err(seal_error)
}

/// What a node is to be told about one workload: the assignment, the plan that becomes applying
/// by being told, and whether a suspension holds the workload stopped.
struct Desired {
    assignment: Assignment,
    plan: Option<Plan>,
    held: bool,
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
    /// What a live assignment holds of its node: the ids it keeps there, its endpoints as placed
    /// there, the challenges its node's proxy answers, and the runs it executes, none while a
    /// suspension holds it stopped.
    async fn place_here(
        &self,
        tx: &mut S::Tx,
        a: &mut Assignment,
        (range, held): (Option<korpis_core::node::advertised::PortRange>, bool),
    ) -> Result<(), AppError> {
        a.ids = ids(tx, a.node, a.workload).await?;
        let now = self.clock.now();
        a.endpoints =
            endpoints::placed(tx, (a.node, a.workload), &a.target.body, range, now).await?;
        a.challenges = tx.challenges_for(a.workload).await?;
        a.alpn_challenges = tx.alpn_challenges_for(a.workload).await?;
        let active = tx.active_runs_within(a.workload).await?;
        if !held {
            a.runs = named(&active, &a.target.body.run_policy());
        }
        a.kept_runs = tx.kept_runs_within(a.workload).await?;
        Ok(())
    }

    /// Whether `workload`'s node relays the control plane's API to it (§9.5 of
    /// docs/08-identity.md): it runs a live installation, or holds a grant in force, whose
    /// authority it has no other way to use. Read from the store at every computation, so an
    /// installation removed, or a grant revoked, expired, or suspended, or the organization it is
    /// scoped in suspended, closes the relay with the next assignment.
    async fn reaches_api(&self, workload: Uuid) -> Result<bool, AppError> {
        let installation = self.store.extension_of_workload(workload).await?;
        if installation.is_some_and(|i| i.uninstalled_at.is_none()) {
            return Ok(true);
        }
        let held = self.store.chains_held_by(&Principal::Workload(workload).key()).await?;
        let now = self.clock.now();
        for chain in held.iter().filter(|chain| in_force(chain, now)) {
            if !self.under_suspension(chain).await? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether any grant of `chain` is scoped in a suspended organization's subtree, which
    /// suspends the chain (§2.1 of docs/08-identity.md). Every link is asked rather than one, so
    /// the answer does not depend on the order the chain is given in: a leaf lies within each of
    /// its ancestors' scopes, so a suspension above any of them reaches it.
    async fn under_suspension(&self, chain: &[GrantRecord]) -> Result<bool, AppError> {
        for link in chain {
            let ancestry = self.store.ancestry(link.grant.scope.anchor()).await?;
            if ancestry.iter().any(|n| n.suspended) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Computes `node`'s set, seals what changed, and returns the set to send.
    pub async fn sync_assignments(&self, node: Uuid) -> Result<AssignmentSet, AppError> {
        let sealer = self
            .assignment_sealer
            .as_ref()
            .ok_or_else(|| AppError::Internal("no assignment sealer is configured".into()))?;
        let desired = self.all_desired(node).await?;
        let range = self.store.node(node).await?.and_then(|n| n.advertised).map(|a| a.ports);
        let mut tx = self.store.begin().await?;
        tx.lock_assignments(node).await?;
        let stored: BTreeMap<Uuid, StoredAssignment> =
            tx.stored_assignments(node).await?.into_iter().map(|s| (s.workload, s)).collect();
        let mut out = Vec::with_capacity(desired.len());
        for Desired { mut assignment, plan, held } in desired {
            if !assignment.residual {
                self.place_here(&mut tx, &mut assignment, (range, held)).await?;
            }
            let digest = sealer.content_digest(&assignment).map_err(seal_error)?;
            let runs = runs_digest(sealer.as_ref(), &assignment)?;
            let kept = stored.get(&assignment.workload);
            let current = match kept {
                Some(s) if s.active && s.content_digest == digest => s.clone(),
                _ => {
                    assignment.generation = kept.map_or(1, |s| s.generation + 1);
                    let signed: Sealed = sealer.seal(&assignment).map_err(seal_error)?;
                    let runs_generation = kept
                        .filter(|s| s.active && s.runs_digest == runs)
                        .map_or(assignment.generation, |s| s.runs_generation);
                    let fresh = StoredAssignment {
                        workload: assignment.workload,
                        generation: assignment.generation,
                        content_digest: digest,
                        runs_digest: runs,
                        runs_generation,
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
    async fn all_desired(&self, node: Uuid) -> Result<Vec<Desired>, AppError> {
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
    /// told; `None` if nothing approved reaches a node yet. While a suspension holds it, it is
    /// told stopped, and a plan waiting to apply waits on, unless it retires the workload; one
    /// already applying is let finish, since a node left half way through its steps would hold
    /// what no plan made (§2.1 of docs/08-identity.md).
    async fn desired(&self, node: Uuid, workload: Uuid) -> Result<Option<Desired>, AppError> {
        let Some(w) = self.store.workload(workload).await? else { return Ok(None) };
        let missing = |what: &str| AppError::Internal(format!("{what} of workload {workload}"));
        let held = self.held_by(w.organization).await?.is_some();
        let plan = active_plan(self.store.open_plans(workload).await?, self.clock.now())
            .filter(|p| !held || p.status == PlanStatus::Applying || retires(p));
        let Some((target, mut power)) = self.told(&w, plan.as_ref()).await? else {
            return Ok(None);
        };
        if held {
            power.power.desired = PowerState::Stopped;
        }
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
        let reaches_api = self.reaches_api(workload).await?;
        let month = korpis_core::tenancy::quota::month_of(self.clock.now()).0;
        let egress = self.store.workload_egress(workload, month).await?;
        let dependencies = self.resolve_dependencies((w.project, w.id), &target.body).await?;
        Ok(Some(Desired {
            assignment: Assignment {
                workload,
                project: w.project,
                organization: w.organization,
                name: w.name.clone(),
                labels: w.labels.clone(),
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
                endpoints: Vec::new(),
                reaches_api,
                egress,
                dependencies,
                challenges: Vec::new(),
                alpn_challenges: Vec::new(),
                runs: Vec::new(),
                kept_runs: Vec::new(),
            },
            plan,
            held,
        }))
    }

    /// The residual assignment of `workload` on `node`: the plan deleting a volume it left there,
    /// with the last target and power it had for the record. No ids, no lease, no recipe it
    /// needs; `None` once nothing is left to do there.
    async fn residual(&self, node: Uuid, workload: Uuid) -> Result<Option<Desired>, AppError> {
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
        Ok(Some(Desired {
            assignment: Assignment {
                workload,
                project: w.project,
                organization: w.organization,
                name: w.name.clone(),
                labels: w.labels.clone(),
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
                endpoints: Vec::new(),
                reaches_api: false,
                egress: None,
                dependencies: Vec::new(),
                challenges: Vec::new(),
                alpn_challenges: Vec::new(),
                runs: Vec::new(),
                kept_runs: Vec::new(),
            },
            plan: Some(plan),
            held: false,
        }))
    }
}
