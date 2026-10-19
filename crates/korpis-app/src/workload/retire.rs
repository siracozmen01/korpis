// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deleting a workload: a retire plan (§3.3 of docs/01-model.md, §4.8 of
//! docs/02-architecture.md). The workload stops, its volumes are detached and retained, and its
//! runtime is removed from its node. A volume is deleted only when the deletion names it, each
//! by its own irreversible step under `volume.delete` (§7 of docs/06-storage.md).
//!
//! A retire plan moves no pointer. Approving it means the workload is being retired: nothing else
//! may be declared or approved for it, and every other proposal is superseded. Its identity is
//! retired (soft-deleted, its name and quota released) when retirement is complete: at once for a
//! workload no node holds, whether nothing was ever approved for it or what was approved no node
//! could take, and otherwise when its node reports the runtime removed. A node removed holds
//! nothing. Approving such a plan checks again that no node holds the workload, since one may
//! have taken it meanwhile, and refuses if one does, so its deletion is proposed again with the
//! steps that node runs.

use super::propose::{self, Proposal};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::Page;
use crate::ports::{Clock, EffectWrite, RecipeSource, RetirementWrite, Store, WorkloadWrite};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::plan::compute::retire_steps;
use korpis_core::workload::plan::impact::delta;
use korpis_core::workload::plan::{PlanStatus, StepExecutor};
use korpis_core::workload::volume::{Volume, VolumeKind};
use korpis_core::workload::{Plan, PlanTarget, PowerState, Workload};
use uuid::Uuid;

const OPERATION: &str = "workload.delete_workload";

/// A workload to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteWorkload {
    /// The workload.
    pub workload: Uuid,
    /// The head intent version the deletion was asked against, if the client states one (§6 of
    /// docs/10-api.md).
    pub expected_version: Option<u64>,
    /// The names of its volumes to delete with it, each named explicitly; the rest are kept.
    pub delete_volumes: Vec<String>,
}

/// The refusal for anything asked of a workload being retired.
pub(super) fn retiring() -> AppError {
    AppError::conflict("workload.retiring", "the workload is being retired")
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Proposes retiring a workload. A retire plan always waits for a human: removing a runtime
    /// is disruptive by the vocabulary's floor, and `workload.delete` is irreversible (§5 of
    /// docs/10-api.md).
    pub async fn delete_workload(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        cmd: DeleteWorkload,
    ) -> Result<Reply<Plan>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let object = ObjectRef::new(Kind::Workload, cmd.workload);
        let (permit, proposing) =
            self.authorize_declaration(caller, "workload.delete", object).await?;
        let w = self.live_workload(cmd.workload).await?;
        if cmd.expected_version.is_some_and(|v| v != w.intent_version) {
            return Err(AppError::conflict(
                "workload.intent_moved",
                format!("the newest declaration is version {}", w.intent_version),
            ));
        }
        let deleting = self.volumes_to_delete(caller, &w, &cmd.delete_volumes).await?;
        let plan = self.retire_plan(caller, &w, (&proposing, &deleting), permit.grant).await?;
        let mut tx = self.store.begin().await?;
        self.write_retirement(&mut tx, caller, &w, &plan, permit.grant).await?;
        finish(tx, caller, idem, OPERATION, plan).await
    }

    /// Proposes `plan`, the retirement of `w`, in `tx`, if `w` has not moved since the plan was
    /// computed and is not being retired already, with the effect that records it under
    /// `grant`.
    pub(crate) async fn write_retirement(
        &self,
        tx: &mut S::Tx,
        caller: &Caller,
        w: &Workload,
        plan: &Plan,
        grant: Uuid,
    ) -> Result<(), AppError> {
        let locked = tx
            .lock_workload(w.id)
            .await?
            .filter(|l| l.deleted_at.is_none())
            .ok_or(AppError::NotFound { kind: "workload" })?;
        if locked.target != w.target || locked.power_target_version != w.power_target_version {
            return Err(AppError::conflict("workload.moved", "the workload changed meanwhile"));
        }
        if tx.retiring(w.id).await? {
            return Err(retiring());
        }
        tx.insert_plan(plan).await?;
        let superseded = tx.supersede_proposed(plan).await?;
        let mut effect =
            Effect::new(caller.principal.clone(), "workload.delete", Outcome::Succeeded)
                .under(grant)
                .after(details([
                    ("approval", propose::approval_name(plan).into()),
                    ("superseded_plans", superseded.into()),
                ]));
        effect.workload = Some(w.id);
        effect.plan = Some(plan.id);
        tx.append_effect(&effect).await?;
        Ok(())
    }

    /// The volumes of `w` that `names` names, each one the caller may delete: attached to `w`,
    /// not deleted, and on the node that holds `w`, which is the one that executes the plan. A
    /// name that is not such a volume is refused, never skipped: deleting data is never guessed.
    async fn volumes_to_delete(
        &self,
        caller: &Caller,
        w: &Workload,
        names: &[String],
    ) -> Result<Vec<Volume>, AppError> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let page = Page::new(None, Page::MAX);
        let attached: Vec<Volume> = self
            .store
            .volumes(w.project, Some(w.id), false, page)
            .await?
            .into_iter()
            .filter(|v| v.attached_to == Some(w.id))
            .collect();
        let node = self.store.placement(w.id).await?.map(|p| p.node);
        let mut deleting = Vec::new();
        for name in names {
            let v = attached.iter().find(|v| &v.name == name).ok_or_else(|| {
                AppError::invalid(
                    "delete_volumes",
                    format!("{name} is not a volume its node has reported for the workload"),
                )
            })?;
            if node != Some(v.node) {
                let why = format!("{name} is not on the node that holds the workload");
                return Err(AppError::conflict("volume.elsewhere", why));
            }
            self.authorize(caller, "volume.delete", ObjectRef::new(Kind::Volume, v.id)).await?;
            if !deleting.iter().any(|d: &Volume| d.id == v.id) {
                deleting.push(v.clone());
            }
        }
        Ok(deleting)
    }

    /// The retire plan from what the workload was approved to be and to do, deleting the
    /// volumes in `deleting`.
    pub(crate) async fn retire_plan(
        &self,
        caller: &Caller,
        w: &Workload,
        (proposing, deleting): (&korpis_core::workload::plan::Proposing, &[Volume]),
        grant: Uuid,
    ) -> Result<Plan, AppError> {
        let held = self.held_by_a_node(w.id).await?;
        let target = match w.target {
            Some(t) => Some(self.store.intent(t).await?.ok_or_else(|| {
                AppError::Internal(format!("workload {} target {t} missing", w.id))
            })?),
            None => None,
        };
        let power = match w.power_target_version {
            0 => PowerState::Stopped,
            v => {
                self.store.power_at(w.id, v).await?.map_or(PowerState::Stopped, |p| p.power.desired)
            }
        };
        let (driver, disruption, wait) = self.running_layers(w).await?;
        // What no node holds has nothing a node could stop or remove.
        let body = target.as_ref().map(|t| &t.body).filter(|_| held);
        let classifier = propose::classifier(&driver, (&disruption, wait), w);
        let steps = retire_steps(w.id, (body, power), deleting, classifier);
        let held = match &target {
            Some(t) => Some(t.body.clone()),
            None => self.store.intent(w.intent).await?.map(|i| i.body),
        };
        let mut cost = delta(held.as_ref(), None);
        // Retained volumes stay; only those deleted leave.
        let declared = |v: &&Volume| v.kind == VolumeKind::Declared;
        cost.volumes = -i64::try_from(deleting.iter().filter(declared).count()).unwrap_or(0);
        let proposal = Proposal {
            exposures: Vec::new(),
            workload: w.id,
            target: PlanTarget::Retire,
            from_intent: w.target,
            from_power: w.power_target_version,
            delta: cost,
            steps,
            proposing,
            grant,
        };
        Ok(propose::plan(caller, proposal, self.clock.now()))
    }

    /// What approving a retire plan does beyond moving its status: every other proposal is
    /// superseded, and a plan the control plane executes alone (nothing ever reached a node) is
    /// applied at once, retiring the identity.
    /// Whether a node not removed holds `workload`: it is placed there, or the node holds an
    /// active assignment for it, or a volume an approved plan of it deletes.
    async fn held_by_a_node(&self, workload: Uuid) -> Result<bool, AppError> {
        for node in self.store.nodes_of(workload).await? {
            let found = self.store.node(node).await?;
            if found.is_some_and(|n| n.admin != korpis_core::node::NodeAdmin::Decommissioned) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) async fn begin_retirement(
        &self,
        tx: &mut S::Tx,
        plan: &mut Plan,
        w: &Workload,
    ) -> Result<(), AppError> {
        tx.supersede_all_proposed(plan).await?;
        if plan.steps.iter().any(|s| s.executor != StepExecutor::ControlPlane) {
            return Ok(());
        }
        if self.held_by_a_node(w.id).await? {
            let why = "a node took it after its deletion was proposed; propose it again";
            return Err(AppError::conflict("workload.moved", why));
        }
        let from = plan.status;
        for to in [PlanStatus::Applying, PlanStatus::Applied] {
            plan.transition(to).map_err(|e| AppError::Internal(e.to_string()))?;
        }
        plan.completed = plan.steps.last().map(|s| s.index);
        let now = self.clock.now();
        if !tx.update_plan(plan, from).await? || !tx.retire_workload(w.id, now).await? {
            return Err(AppError::conflict("workload.moved", "the workload changed meanwhile"));
        }
        self.place_target(tx, w, None).await?;
        let mut e = Effect::new(
            Principal::System(SystemActor::Reconciler),
            "workload.retire",
            Outcome::Succeeded,
        );
        e.workload = Some(w.id);
        e.plan = Some(plan.id);
        e.step = plan.completed;
        tx.append_effect(&e).await?;
        Ok(())
    }
}
