// SPDX-License-Identifier: AGPL-3.0-or-later

//! Who may decide a plan (§3.3 of docs/01-model.md, §3.6 of docs/08-identity.md).
//!
//! A plan that needs **confirmation** was proposed under a grant that permits its action; it
//! waits only so that a human sees it. Its proposer may confirm it, re-checked now, and so may
//! anyone else who may perform its action or holds `plan.approve` over the workload. A plan that
//! needs **authority** was proposed under a grant that may only propose: approving it is
//! `may_approve`, which no subject can satisfy for their own plan.

use super::Snapshot;
use super::gateway::core_action;
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::authz::evaluate::{Approval as Request, may_approve};
use korpis_core::authz::{Decision, Grant, Kind, ObjectRef, Permit, Scope};
use korpis_core::workload::Plan;
use korpis_core::workload::plan::{Approval, StepAction};

/// The action a plan performs, which a confirmer must be permitted.
fn plan_action(plan: &Plan) -> &'static str {
    let started = |a| plan.steps.iter().any(|s| s.action == a);
    if matches!(plan.target, korpis_core::workload::PlanTarget::Power(_)) {
        if started(StepAction::Stop) {
            return "workload.stop";
        }
        if started(StepAction::Restart) {
            return "workload.restart";
        }
        return "workload.start";
    }
    if plan.target == korpis_core::workload::PlanTarget::Retire {
        return "workload.delete";
    }
    "workload.write"
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Authorizes `caller` to approve or reject `plan`, recording a refusal. The permit names
    /// the grant the decision is made under, which its effect records.
    pub(crate) async fn authorize_plan_decision(
        &self,
        caller: &Caller,
        plan: &Plan,
    ) -> Result<Permit, AppError> {
        let workload = ObjectRef::new(Kind::Workload, plan.workload);
        match &plan.approval {
            Approval::Authority { scope } => self.authorize_authority(caller, plan, scope).await,
            Approval::Auto | Approval::Confirmation => {
                let action = core_action(plan_action(plan))?;
                if let Decision::Permit(p) = self.decide(caller, &action, workload, None).await? {
                    return Ok(p);
                }
                self.authorize(caller, "plan.approve", workload).await
            }
        }
    }

    async fn authorize_authority(
        &self,
        caller: &Caller,
        plan: &Plan,
        scope: &Scope,
    ) -> Result<Permit, AppError> {
        let mut snapshot = Snapshot::default();
        snapshot.load(&self.store, scope.anchor()).await?;
        let proposing: Vec<Grant> = self
            .store
            .grant_chain(plan.proposed_under)
            .await?
            .into_iter()
            .map(|r| r.grant)
            .collect();
        let mut chains = Vec::new();
        for records in self.store.chains_held_by(&caller.subject.key).await? {
            let chain: Vec<Grant> = records.into_iter().map(|r| r.grant).collect();
            for g in &chain {
                snapshot.load_grant(&self.store, g).await?;
            }
            chains.push(chain);
        }
        let proposer = plan.computed_by.subject();
        let request = Request {
            approver: &caller.subject,
            approver_chains: &chains,
            proposer: &proposer,
            proposing_chain: &proposing,
            approval_scope: scope,
            source_ip: caller.source_ip,
            mfa: caller.mfa,
            now: self.clock.now(),
        };
        match may_approve(&request, &snapshot) {
            Decision::Permit(p) => Ok(p),
            Decision::Propose { .. } | Decision::Deny(_) => {
                let action = core_action("plan.approve")?;
                Err(self
                    .refuse(caller, &action, ObjectRef::new(Kind::Workload, plan.workload))
                    .await)
            }
        }
    }
}
