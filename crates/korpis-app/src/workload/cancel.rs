// SPDX-License-Identifier: AGPL-3.0-or-later

//! Cancelling a plan (§7 of docs/03-state.md), and reading plans.
//!
//! A proposed plan is withdrawn. An approved or queued plan none of whose steps has completed is
//! withdrawn too, and the targets it moved are put back, so cancelling it leaves the workload as
//! if it had never been approved. An applying plan is cancelled by lowering `through` on the
//! nodes executing it, which arrives with the agent protocol.

use super::decide::effect;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{Clock, EffectWrite, RecipeSource, Store, WorkloadWrite};
use korpis_core::audit::details;
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::plan::PlanStatus;
use korpis_core::workload::{Plan, PlanTarget, Workload};
use uuid::Uuid;

const CANCEL: &str = "plan.cancel_plan";

/// Whether `plan` is what moved the workload's targets to where they are now.
fn moved_last(plan: &Plan, w: &Workload) -> bool {
    match plan.target {
        PlanTarget::Intent(i) => w.target == Some(i),
        PlanTarget::Power(v) => w.power_target_version == v,
        PlanTarget::Retire | PlanTarget::Migrate => false,
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Cancels a plan.
    pub async fn cancel_plan(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        id: Uuid,
    ) -> Result<Reply<Plan>, AppError> {
        if let Some(r) = self.replay(caller, idem, CANCEL).await? {
            return Ok(Reply::Replayed(r));
        }
        let plan = self.store.plan(id).await?.ok_or(AppError::NotFound { kind: "plan" })?;
        let object = ObjectRef::new(Kind::Workload, plan.workload);
        let permit = self.authorize(caller, "plan.cancel", object).await?;
        let mut tx = self.store.begin().await?;
        let mut plan = tx.lock_plan(id).await?.ok_or(AppError::NotFound { kind: "plan" })?;
        let w = tx
            .lock_workload(plan.workload)
            .await?
            .ok_or(AppError::NotFound { kind: "workload" })?;
        let from = plan.status;
        match from {
            PlanStatus::Proposed => {}
            PlanStatus::Approved | PlanStatus::Queued if plan.completed.is_none() => {
                // Only the plan that moved the targets last can put them back; a later one has
                // moved them since, and restoring would undo it.
                let restored = moved_last(&plan, &w)
                    && tx
                        .restore_targets(w.id, plan.from_intent, plan.from_power, w.target_version)
                        .await?;
                if !restored {
                    return Err(AppError::conflict(
                        "plan.not_latest",
                        "a later approval has moved the workload since this plan",
                    ));
                }
            }
            PlanStatus::Applying | PlanStatus::Approved | PlanStatus::Queued => {
                return Err(AppError::NotYet {
                    what: "cancelling a plan nodes have begun",
                    phase: 0,
                });
            }
            terminal => {
                return Err(AppError::conflict(
                    "plan.finished",
                    format!("the plan is {}", terminal.as_str()),
                ));
            }
        }
        plan.transition(PlanStatus::Cancelled).map_err(|e| AppError::Internal(e.to_string()))?;
        if !tx.update_plan(&plan, from).await? {
            return Err(AppError::conflict("plan.not_proposed", "the plan changed meanwhile"));
        }
        let after = details([("was", from.as_str().into())]);
        tx.append_effect(
            &effect(caller.principal.clone(), "plan.cancel", &plan, after).under(permit.grant),
        )
        .await?;
        finish(tx, caller, idem, CANCEL, plan).await
    }

    /// One plan the caller may read.
    pub async fn get_plan(&self, caller: &Caller, id: Uuid) -> Result<Plan, AppError> {
        let plan = self.store.plan(id).await?.ok_or(AppError::NotFound { kind: "plan" })?;
        let object = ObjectRef::new(Kind::Workload, plan.workload);
        self.authorize(caller, "plan.read", object).await?;
        Ok(plan)
    }

    /// The plans of a workload whose plans the caller may read, oldest first.
    pub async fn list_plans(
        &self,
        caller: &Caller,
        workload: Uuid,
        page: Page,
    ) -> Result<Paged<Plan>, AppError> {
        self.authorize(caller, "plan.read", ObjectRef::new(Kind::Workload, workload)).await?;
        let items = self.store.plans(workload, page).await?;
        let next = page.next(&items, |p| p.id);
        Ok(Paged { items, next })
    }
}
