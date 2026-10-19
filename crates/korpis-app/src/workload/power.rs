// SPDX-License-Identifier: AGPL-3.0-or-later

//! Start, stop and restart: a new power declaration and the plan toward it (§3.3 of
//! docs/01-model.md). The intent chain is untouched; the power head moves at declaration and the
//! power target, which nodes are told, only when the plan is approved.

use super::propose::{self, Proposal};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, RecipeSource, RetirementWrite, Store, WorkloadWrite};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef, Permit};
use korpis_core::recipe::Disruption;
use korpis_core::workload::plan::compute::power_steps;
use korpis_core::workload::plan::{Approval, DriverClass, Proposing, ResourceDelta};
use korpis_core::workload::{
    Plan, PlanTarget, Power, PowerChange, PowerRecord, PowerState, Workload,
};
use std::time::Duration;
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
    /// Persist everything and approve nothing: the plan waits for its proposer to confirm it
    /// even when default policy would approve it at once (§3.3 of docs/01-model.md).
    pub propose: bool,
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

/// The effect a power change records: who changed it, to what, under which grant, and whether
/// its plan was approved at once.
pub(crate) fn power_effect(
    caller: &Caller,
    change: PowerChange,
    p: &Prepared,
    (grant, approved): (Uuid, bool),
) -> Effect {
    let mut effect = Effect::new(caller.principal.clone(), action(change), Outcome::Succeeded)
        .under(grant)
        .after(details([
            ("power_version", p.power.power.version.into()),
            ("cycle", p.power.power.cycle.into()),
            ("approved", approved.into()),
        ]));
    effect.workload = Some(p.power.workload);
    effect.plan = Some(p.plan.id);
    effect
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

/// A power change computed against what the store said when it was prepared, not yet written.
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    /// The workload as it was read.
    workload: Workload,
    /// The power declared before this change: the head it was made against.
    pub(crate) head: Power,
    /// The new power declaration.
    pub(crate) power: PowerRecord,
    /// The plan toward it.
    pub(crate) plan: Plan,
}

/// What writing a prepared power change did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Written {
    /// Written; whether its plan was approved at once.
    Done {
        /// Whether nothing in it needed a human, so it was approved.
        approved: bool,
    },
    /// The workload's power moved since it was prepared; nothing was written.
    Moved(Box<Workload>),
    /// The workload is being retired; nothing was written.
    Retiring,
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
        let mut p = self.prepare_power(caller, w, cmd.change, (&permit, &proposing)).await?;
        if cmd.propose && p.plan.approval == Approval::Auto {
            p.plan.approval = Approval::Confirmation;
            p.plan.approval_reason = "its proposer asked to confirm it".into();
        }
        let mut tx = self.store.begin().await?;
        let approved = match self.write_power(&mut tx, caller, &mut p).await? {
            Written::Done { approved } => approved,
            Written::Moved(locked) => return Err(power_moved(&locked)),
            Written::Retiring => return Err(super::retire::retiring()),
        };
        let effect = power_effect(caller, cmd.change, &p, (permit.grant, approved));
        tx.append_effect(&effect).await?;
        let Prepared { power, plan, .. } = p;
        finish(tx, caller, idem, OPERATION, PowerChanged { power, plan }).await
    }

    /// The declaration `change` makes of `w`, and the plan toward it, under `authority`.
    pub(crate) async fn prepare_power(
        &self,
        caller: &Caller,
        w: Workload,
        change: PowerChange,
        (permit, proposing): (&Permit, &Proposing),
    ) -> Result<Prepared, AppError> {
        let head = self.power_record(w.id, w.power_version).await?;
        let power = PowerRecord {
            workload: w.id,
            power: head.power.next(change),
            set_by: caller.principal.clone(),
            set_at: self.clock.now(),
        };
        let plan = self.power_plan(caller, &w, &power, proposing, permit).await?;
        Ok(Prepared { workload: w, head: head.power, power, plan })
    }

    /// Writes `p` in `tx` if the workload's power is still what it was prepared against, and
    /// approves its plan at once where nothing in it needs a human.
    pub(crate) async fn write_power<T: WorkloadWrite + RetirementWrite>(
        &self,
        tx: &mut T,
        caller: &Caller,
        p: &mut Prepared,
    ) -> Result<Written, AppError> {
        let w = &p.workload;
        let locked =
            tx.lock_workload(w.id).await?.ok_or(AppError::NotFound { kind: "workload" })?;
        if tx.retiring(w.id).await? {
            return Ok(Written::Retiring);
        }
        if locked.power_version != w.power_version
            || locked.power_target_version != w.power_target_version
            || !tx.append_power(&p.power).await?
        {
            return Ok(Written::Moved(Box::new(locked)));
        }
        tx.insert_plan(&p.plan).await?;
        tx.supersede_proposed(&p.plan).await?;
        let from = p.plan.status;
        let approved = propose::approve_if_auto(tx, &mut p.plan, caller, self.clock.now()).await?;
        let version = p.power.power.version;
        if approved
            && (!tx.update_plan(&p.plan, from).await?
                || !tx.move_power_target(w.id, version, w.power_target_version).await?)
        {
            return Ok(Written::Moved(Box::new(locked)));
        }
        Ok(Written::Done { approved })
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
        let (driver, disruption, wait) = self.running_layers(w).await?;
        let classifier = propose::classifier(&driver, (&disruption, wait), w);
        let steps = power_steps(w.id, from, to.power, classifier);
        let proposal = Proposal {
            exposures: Vec::new(),
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

    /// The driver's and the recipe's layers of the classification chain for what the workload
    /// runs, its target or its head while nothing is approved, and how long its recipe's hook
    /// waits before a stop is signalled.
    pub(super) async fn running_layers(
        &self,
        w: &Workload,
    ) -> Result<(DriverClass, Disruption, Duration), AppError> {
        let id = w.target.unwrap_or(w.intent);
        let intent = self
            .store
            .intent(id)
            .await?
            .ok_or_else(|| AppError::Internal(format!("intent {id}")))?;
        let driver = self.driver_class(w.organization, intent.body.runtime.tier).await?;
        let r = &intent.body.recipe;
        let (_, recipe) = self.resolve_recipe(&r.name, &r.digest).await?;
        Ok((driver, recipe.disruption, recipe.lifecycle_hooks.stop_wait()))
    }
}
