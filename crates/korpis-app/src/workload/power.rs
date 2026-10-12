// SPDX-License-Identifier: AGPL-3.0-or-later

//! Start, stop and restart: a new power declaration and the plan toward it (§3.3 of
//! docs/01-model.md). The intent chain is untouched; the power head moves at declaration and the
//! power target, which nodes are told, only when the plan is approved.

use super::propose::{self, Proposal};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, RecipeSource, Store, WorkloadWrite};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef, Permit};
use korpis_core::recipe::Disruption;
use korpis_core::workload::plan::compute::power_steps;
use korpis_core::workload::plan::{DriverClass, Proposing, ResourceDelta};
use korpis_core::workload::{
    Plan, PlanTarget, Power, PowerChange, PowerRecord, PowerState, Workload,
};
use uuid::Uuid;

const OPERATION: &str = "workload.change_power";

/// A power change asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangePower {
    /// The workload.
    pub workload: Uuid,
    /// Start, stop, or restart.
    pub change: PowerChange,
    /// The power head's version the change was made against (§6 of docs/10-api.md).
    pub expected_version: u64,
}

/// What a power change produced.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerChanged {
    /// The new power declaration.
    pub power: PowerRecord,
    /// The plan toward it.
    pub plan: Plan,
}

fn action(change: PowerChange) -> &'static str {
    match change {
        PowerChange::Start => "workload.start",
        PowerChange::Stop => "workload.stop",
        PowerChange::Restart => "workload.restart",
    }
}

fn power_moved(w: &Workload) -> AppError {
    AppError::conflict(
        "workload.power_moved",
        format!(
            "the workload's power moved: the newest declaration is version {}",
            w.power_version
        ),
    )
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Declares a power change. Like a declaration of intent, a propose-only grant yields a plan
    /// that waits, and a plan nothing in which needs a human is approved at once.
    pub async fn change_power(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, PowerChanged>,
        cmd: ChangePower,
    ) -> Result<Reply<PowerChanged>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let object = ObjectRef::new(Kind::Workload, cmd.workload);
        let (permit, proposing) =
            self.authorize_declaration(caller, action(cmd.change), object).await?;
        let w = self.live_workload(cmd.workload).await?;
        if w.power_version != cmd.expected_version {
            return Err(power_moved(&w));
        }
        let head = self.power_record(w.id, w.power_version).await?;
        let power = PowerRecord {
            workload: w.id,
            power: head.power.next(cmd.change),
            set_by: caller.principal.clone(),
            set_at: self.clock.now(),
        };
        let mut plan = self.power_plan(caller, &w, &power, &proposing, &permit).await?;
        let mut tx = self.store.begin().await?;
        let locked =
            tx.lock_workload(w.id).await?.ok_or(AppError::NotFound { kind: "workload" })?;
        if locked.power_version != w.power_version
            || locked.power_target_version != w.power_target_version
            || !tx.append_power(&power).await?
        {
            return Err(power_moved(&locked));
        }
        tx.insert_plan(&plan).await?;
        tx.supersede_proposed(&plan).await?;
        let from = plan.status;
        let approved =
            propose::approve_if_auto(&mut tx, &mut plan, caller, self.clock.now()).await?;
        if approved
            && (!tx.update_plan(&plan, from).await?
                || !tx.move_power_target(w.id, power.power.version, w.power_target_version).await?)
        {
            return Err(power_moved(&locked));
        }
        let mut effect =
            Effect::new(caller.principal.clone(), action(cmd.change), Outcome::Succeeded)
                .under(permit.grant)
                .after(details([
                    ("power_version", power.power.version.into()),
                    ("cycle", power.power.cycle.into()),
                    ("approved", approved.into()),
                ]));
        effect.workload = Some(w.id);
        effect.plan = Some(plan.id);
        tx.append_effect(&effect).await?;
        finish(tx, caller, idem, OPERATION, PowerChanged { power, plan }).await
    }

    /// A workload that exists and is not deleted.
    pub(super) async fn live_workload(&self, id: Uuid) -> Result<Workload, AppError> {
        self.store
            .workload(id)
            .await?
            .filter(|w| w.deleted_at.is_none())
            .ok_or(AppError::NotFound { kind: "workload" })
    }

    async fn power_record(&self, workload: Uuid, version: u64) -> Result<PowerRecord, AppError> {
        self.store
            .power_at(workload, version)
            .await?
            .ok_or_else(|| AppError::Internal(format!("workload {workload} power {version}")))
    }

    /// The plan from the approved power to `to`, classified by the recipe the workload runs.
    async fn power_plan(
        &self,
        caller: &Caller,
        w: &Workload,
        to: &PowerRecord,
        proposing: &Proposing,
        permit: &Permit,
    ) -> Result<Plan, AppError> {
        let from = match w.power_target_version {
            0 => Power { desired: PowerState::Stopped, cycle: 0, version: 0 },
            v => self.power_record(w.id, v).await?.power,
        };
        let disruption = self.running_disruption(w).await?;
        let driver = DriverClass::default();
        let steps = power_steps(w.id, from, to.power, propose::classifier(&driver, &disruption));
        let proposal = Proposal {
            workload: w.id,
            target: PlanTarget::Power(to.power.version),
            from_intent: w.target,
            from_power: w.power_target_version,
            delta: ResourceDelta::default(),
            steps,
            proposing,
            grant: permit.grant,
        };
        Ok(propose::plan(caller, proposal, self.clock.now()))
    }

    /// What interrupting the workload costs, by the recipe of what it runs: its target, or its
    /// head while nothing is approved.
    async fn running_disruption(&self, w: &Workload) -> Result<Disruption, AppError> {
        let id = w.target.unwrap_or(w.intent);
        let intent = self
            .store
            .intent(id)
            .await?
            .ok_or_else(|| AppError::Internal(format!("intent {id}")))?;
        let r = &intent.body.recipe;
        let (_, recipe) = self.resolve_recipe(&r.name, &r.digest).await?;
        Ok(recipe.disruption)
    }
}
