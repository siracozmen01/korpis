// SPDX-License-Identifier: AGPL-3.0-or-later

//! Proposing a prune (§5.3 of docs/06-storage.md): the repository's retention policy judges the
//! workload's backups there now, and the plan's one step names the backups it forgets and why
//! each other one is kept. It is irreversible, so it is never approved by default; the workload
//! may be retired, since its backups outlive it.

use super::propose::{self, Proposal, approve_if_auto};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite as _, RecipeSource, Store, WorkloadWrite as _};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::backup::retention;
use korpis_core::recipe::Disruption;
use korpis_core::workload::plan::impact::ResourceDelta;
use korpis_core::workload::plan::prune::{forgotten, prune_steps};
use korpis_core::workload::plan::step::DriverClass;
use korpis_core::workload::{Plan, PlanTarget};
use uuid::Uuid;

const OPERATION: &str = "backup.prune";

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Proposes pruning `workload`'s backups in `repository` under `backup.prune` on the
    /// workload.
    pub async fn propose_prune(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        (workload, repository): (Uuid, Uuid),
    ) -> Result<Reply<Plan>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let object = ObjectRef::new(Kind::Workload, workload);
        let (permit, proposing) = self.authorize_declaration(caller, OPERATION, object).await?;
        let w =
            self.store.workload(workload).await?.ok_or(AppError::NotFound { kind: "workload" })?;
        let repo = self.store.repository(repository).await?.filter(|r| r.deleted_at.is_none());
        let repo = repo.ok_or(AppError::NotFound { kind: "repository" })?;
        if repo.organization != w.organization {
            return Err(AppError::NotFound { kind: "repository" });
        }
        let open = self.store.open_plans(w.id).await?;
        if open.iter().any(|p| p.target == PlanTarget::Prune(repo.id)) {
            return Err(AppError::conflict("prune.busy", "a prune of these backups is open"));
        }
        let now = self.clock.now();
        let candidates = self.store.prune_candidates((repo.id, w.id)).await?;
        let read = self.store.backups_being_read((repo.id, w.id), now).await?;
        let judged = retention::judge(&repo.retention, &candidates, &read, now);
        if judged.iter().all(|j| !j.kept.is_empty()) {
            let why = "every backup is kept: by the policy, its floor, or because it is being read";
            return Err(AppError::conflict("prune.nothing", why));
        }
        let (driver, recipe) = (DriverClass::default(), Disruption::default());
        let steps = prune_steps(w.id, repo.id, &judged, propose::classifier(&driver, &recipe, &w));
        let proposal = Proposal {
            exposures: Vec::new(),
            workload: w.id,
            target: PlanTarget::Prune(repo.id),
            from_intent: w.target,
            from_power: w.power_target_version,
            delta: ResourceDelta::default(),
            steps,
            proposing: &proposing,
            grant: permit.grant,
        };
        let mut plan = propose::plan(caller, proposal, now);
        let mut tx = self.store.begin().await?;
        tx.insert_plan(&plan).await?;
        if approve_if_auto(&mut tx, &mut plan, caller, now).await? {
            tx.update_plan(&plan, korpis_core::workload::plan::PlanStatus::Proposed).await?;
        }
        let forgets = plan.steps.first().map_or(0, |s| forgotten(s).len());
        let after = details([
            ("repository", repo.id.to_string().into()),
            ("forget", forgets.into()),
            ("approval", propose::approval_name(&plan).into()),
        ]);
        let mut e = Effect::new(caller.principal.clone(), OPERATION, Outcome::Succeeded)
            .under(permit.grant)
            .after(after);
        e.workload = Some(w.id);
        e.plan = Some(plan.id);
        tx.append_effect(&e).await?;
        finish(tx, caller, idem, OPERATION, plan).await
    }
}
