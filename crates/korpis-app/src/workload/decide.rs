// SPDX-License-Identifier: AGPL-3.0-or-later

//! Approving and rejecting a plan (§3.3 of docs/01-model.md, §3.2 and §7 of docs/03-state.md).
//! Approving moves the target, or the power target, by compare-and-swap, and only a plan for the
//! head, computed against the current target, can be approved: a plan the world moved under is
//! superseded, and a plan nobody approved in time has expired.

use super::retire::retiring;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, RecipeSource, RetirementWrite, Store, Tx, WorkloadWrite};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::plan::PlanStatus;
use korpis_core::workload::{Plan, PlanTarget, Workload};
use std::time::SystemTime;
use uuid::Uuid;

const APPROVE: &str = "plan.approve_plan";
const REJECT: &str = "plan.reject_plan";

/// A plan to approve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovePlan {
    /// The plan.
    pub plan: Uuid,
    /// Approve now, apply in a window that opens then.
    pub apply_after: Option<SystemTime>,
}

/// A plan to reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectPlan {
    /// The plan.
    pub plan: Uuid,
    /// Why, for the proposer.
    pub reason: String,
}

/// Whether `plan` is still for the workload as it is: for its head, computed against its target.
fn current(plan: &Plan, w: &Workload) -> Result<bool, AppError> {
    Ok(match plan.target {
        PlanTarget::Intent(i) => i == w.intent && plan.from_intent == w.target,
        PlanTarget::Power(v) => v == w.power_version && plan.from_power == w.power_target_version,
        // A retire plan's steps were computed from what the workload was approved to be and do.
        PlanTarget::Retire => {
            plan.from_intent == w.target && plan.from_power == w.power_target_version
        }
        PlanTarget::Migrate => {
            return Err(AppError::NotYet { what: "migration plans", phase: 3 });
        }
        // Deleting a retained volume asks nothing of the workload, retired or not; whether the
        // volume can still be deleted is read where the plan is approved. Neither does pruning
        // its backups, which outlive it.
        PlanTarget::Volume(..) | PlanTarget::Prune(_) => true,
    })
}

pub(super) fn effect(
    actor: Principal,
    action: &str,
    plan: &Plan,
    after: serde_json::Map<String, serde_json::Value>,
) -> Effect {
    let mut e = Effect::new(actor, action, Outcome::Succeeded).after(after);
    e.workload = Some(plan.workload);
    e.plan = Some(plan.id);
    e
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// A plan, locked with its workload for the rest of the transaction; only a proposed plan
    /// can be decided.
    pub(super) async fn lock_proposed(
        &self,
        tx: &mut S::Tx,
        id: Uuid,
    ) -> Result<(Plan, Workload), AppError> {
        let plan = tx.lock_plan(id).await?.ok_or(AppError::NotFound { kind: "plan" })?;
        let w = tx
            .lock_workload(plan.workload)
            .await?
            .ok_or(AppError::NotFound { kind: "workload" })?;
        if plan.status != PlanStatus::Proposed {
            return Err(AppError::conflict(
                "plan.not_proposed",
                format!("the plan is {}", plan.status.as_str()),
            ));
        }
        Ok((plan, w))
    }

    /// Marks a plan that can no longer be approved, commits that, and reports why.
    async fn retire_stale(&self, mut tx: S::Tx, mut plan: Plan, to: PlanStatus) -> AppError {
        let reason = if to == PlanStatus::Expired { "plan.expired" } else { "plan.superseded" };
        let record = async {
            plan.transition(to).map_err(|e| AppError::Internal(e.to_string()))?;
            tx.update_plan(&plan, PlanStatus::Proposed).await?;
            let actor = Principal::System(SystemActor::Reconciler);
            let action = if to == PlanStatus::Expired { "plan.expire" } else { "plan.supersede" };
            tx.append_effect(&effect(actor, action, &plan, details([]))).await?;
            tx.commit().await?;
            Ok::<_, AppError>(())
        };
        match record.await {
            Ok(()) => AppError::conflict(reason, format!("the plan is now {}", to.as_str())),
            Err(e) => e,
        }
    }

    /// What `plan` has become if it can no longer be approved for `w` at `now`: expired, or
    /// superseded because the workload moved or was retired, or the volume it operates on went
    /// or changed hands. A plan on a volume may belong to a retired workload.
    async fn stale(
        &self,
        plan: &Plan,
        w: &Workload,
        now: SystemTime,
    ) -> Result<Option<PlanStatus>, AppError> {
        if !plan.approvable(now) {
            return Ok(Some(PlanStatus::Expired));
        }
        let for_volume = matches!(plan.target, PlanTarget::Volume(..) | PlanTarget::Prune(_));
        if (w.deleted_at.is_some() && !for_volume) || !current(plan, w)? {
            return Ok(Some(PlanStatus::Superseded));
        }
        if let PlanTarget::Volume(v, op) = plan.target
            && !self.volume_plan_current(plan, (v, op)).await?
        {
            return Ok(Some(PlanStatus::Superseded));
        }
        Ok(None)
    }

    /// Approves a plan, or confirms it (§3.3 of docs/01-model.md).
    pub async fn approve_plan(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        cmd: ApprovePlan,
    ) -> Result<Reply<Plan>, AppError> {
        if let Some(r) = self.replay(caller, idem, APPROVE).await? {
            return Ok(Reply::Replayed(r));
        }
        let plan = self.store.plan(cmd.plan).await?.ok_or(AppError::NotFound { kind: "plan" })?;
        let permit = self.authorize_plan_decision(caller, &plan).await?;
        let screened = self.screen_plan(&plan).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let (mut plan, w) = self.lock_proposed(&mut tx, cmd.plan).await?;
        if let Some(stale) = self.stale(&plan, &w, now).await? {
            return Err(self.retire_stale(tx, plan, stale).await);
        }
        if tx.retiring(w.id).await? {
            return Err(retiring());
        }
        let to =
            if tx.plan_applying(w.id).await? { PlanStatus::Queued } else { PlanStatus::Approved };
        plan.transition(PlanStatus::Approved).map_err(|e| AppError::Internal(e.to_string()))?;
        if to == PlanStatus::Queued {
            plan.transition(to).map_err(|e| AppError::Internal(e.to_string()))?;
        }
        plan.approved_by = Some(caller.principal.clone());
        plan.approved_at = Some(now);
        plan.apply_after = cmd.apply_after;
        let moved = tx.update_plan(&plan, PlanStatus::Proposed).await?
            && match plan.target {
                PlanTarget::Intent(i) => tx.move_target(w.id, i, w.target_version).await?,
                PlanTarget::Power(v) => {
                    tx.move_power_target(w.id, v, w.power_target_version).await?
                }
                PlanTarget::Retire | PlanTarget::Volume(..) | PlanTarget::Prune(_) => true,
                PlanTarget::Migrate => false,
            };
        if !moved {
            return Err(AppError::conflict(
                "plan.superseded",
                "the workload moved during approval",
            ));
        }
        let after = details([("status", plan.status.as_str().into())]);
        tx.append_effect(
            &effect(caller.principal.clone(), "plan.approve", &plan, after).under(permit.grant),
        )
        .await?;
        match plan.target {
            PlanTarget::Intent(i) => {
                let target =
                    self.store.intent(i).await?.ok_or(AppError::NotFound { kind: "intent" })?;
                self.claim_target(&mut tx, &w, &target.body).await?;
                self.place_screened(&mut tx, &w, Some(&target), &screened).await?;
            }
            PlanTarget::Retire => self.begin_retirement(&mut tx, &mut plan, &w).await?,
            PlanTarget::Power(_)
            | PlanTarget::Migrate
            | PlanTarget::Volume(..)
            | PlanTarget::Prune(_) => {}
        }
        finish(tx, caller, idem, APPROVE, plan).await
    }

    /// Rejects a proposed plan. Nothing moves; the declaration it was for stays declared.
    pub async fn reject_plan(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        cmd: RejectPlan,
    ) -> Result<Reply<Plan>, AppError> {
        if let Some(r) = self.replay(caller, idem, REJECT).await? {
            return Ok(Reply::Replayed(r));
        }
        let plan = self.store.plan(cmd.plan).await?.ok_or(AppError::NotFound { kind: "plan" })?;
        let permit = self.authorize_plan_decision(caller, &plan).await?;
        let mut tx = self.store.begin().await?;
        let (mut plan, _) = self.lock_proposed(&mut tx, cmd.plan).await?;
        plan.transition(PlanStatus::Rejected).map_err(|e| AppError::Internal(e.to_string()))?;
        if !tx.update_plan(&plan, PlanStatus::Proposed).await? {
            return Err(AppError::conflict("plan.not_proposed", "the plan was decided meanwhile"));
        }
        let after = details([("reason", cmd.reason.into())]);
        tx.append_effect(
            &effect(caller.principal.clone(), "plan.reject", &plan, after).under(permit.grant),
        )
        .await?;
        finish(tx, caller, idem, REJECT, plan).await
    }
}
