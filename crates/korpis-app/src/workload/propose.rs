// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plans as a declaration produces them: computed, classified, and, when nothing in them needs a
//! human, approved in the same transaction (§3.3 of docs/01-model.md).

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store, WorkloadWrite};
use korpis_core::recipe::Disruption;
use korpis_core::workload::plan::impact::assess;
use korpis_core::workload::plan::{
    Approval, Classifier, DriverClass, PROPOSAL_LIFETIME, PlanStatus, Proposing, ResourceDelta,
    Step, approval,
};
use korpis_core::workload::spec::IsolationTier;
use korpis_core::workload::{Plan, PlanTarget, Workload};
use std::time::SystemTime;
use uuid::Uuid;

/// Everything a plan is computed from.
pub(super) struct Proposal<'a> {
    pub workload: Uuid,
    pub target: PlanTarget,
    /// The workload's target intent when computed.
    pub from_intent: Option<Uuid>,
    /// The workload's power target when computed, by version.
    pub from_power: u64,
    /// How reservations change.
    pub delta: ResourceDelta,
    /// What it does to the exposure of each endpoint it creates or changes.
    pub exposures: Vec<korpis_core::workload::endpoint::ExposureImpact>,
    pub steps: Vec<Step>,
    pub proposing: &'a Proposing,
    pub grant: Uuid,
}

/// How a step is classified for a workload of this recipe on these drivers, with the operator's
/// overrides of the workload (§3.3 of docs/01-model.md).
pub(super) fn classifier<'a>(
    driver: &'a DriverClass,
    recipe: &'a Disruption,
    w: &'a Workload,
) -> Classifier<'a> {
    Classifier { driver, recipe, overrides: &w.classification }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The driver's layer of the classification chain for a workload of `organization` at
    /// `tier`: what every driver that could run it there makes disruptive (§3.3 of
    /// docs/01-model.md).
    pub(super) async fn driver_class(
        &self,
        organization: Uuid,
        tier: IsolationTier,
    ) -> Result<DriverClass, AppError> {
        let owners = self.owners(organization).await?;
        let drivers = self.store.serving_drivers(&owners).await?;
        Ok(DriverClass::of(&drivers, tier))
    }
}

/// The plan for a proposal, proposed by `caller` at `now`.
pub(super) fn plan(caller: &Caller, p: Proposal<'_>, now: SystemTime) -> Plan {
    let mut impact = assess(&p.steps, p.delta);
    impact.exposures = p.exposures;
    let (approval, approval_reason) = approval::required(&p.steps, &impact, p.proposing);
    Plan {
        id: Uuid::now_v7(),
        workload: p.workload,
        target: p.target,
        operation: None,
        from_intent: p.from_intent,
        from_power: p.from_power,
        computed_at: now,
        computed_by: caller.principal.clone(),
        proposed_under: p.grant,
        steps: p.steps,
        impact,
        approval,
        approval_reason,
        status: PlanStatus::Proposed,
        completed: None,
        apply_after: None,
        expires_at: now + PROPOSAL_LIFETIME,
        approved_by: None,
        approved_at: None,
    }
}

/// Approves an auto-approvable plan now, by default policy: it becomes approved, or queued
/// behind a plan already applying (§7 of docs/03-state.md). Returns whether it was.
pub(super) async fn approve_if_auto<T: WorkloadWrite>(
    tx: &mut T,
    plan: &mut Plan,
    caller: &Caller,
    now: SystemTime,
) -> Result<bool, AppError> {
    if plan.approval != Approval::Auto {
        return Ok(false);
    }
    let to = if tx.plan_applying(plan.workload).await? {
        PlanStatus::Queued
    } else {
        PlanStatus::Approved
    };
    plan.transition(PlanStatus::Approved).map_err(|e| AppError::Internal(e.to_string()))?;
    if to == PlanStatus::Queued {
        plan.transition(to).map_err(|e| AppError::Internal(e.to_string()))?;
    }
    plan.approved_by = Some(caller.principal.clone());
    plan.approved_at = Some(now);
    Ok(true)
}

/// The name of the approval a plan needs, as effects record it.
pub(super) fn approval_name(p: &Plan) -> &'static str {
    match p.approval {
        Approval::Auto => "auto",
        Approval::Confirmation => "confirmation",
        Approval::Authority { .. } => "authority",
    }
}
