// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading runs and cancelling one (§4.4 of docs/05-scheduling.md). A run needs no action of its
//! own (§5 of docs/10-api.md): it is read under `workload.read` on its workload and cancelled
//! under `workload.stop`, since cancelling one stops what it runs.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{
    Clock, EffectWrite as _, RecipeSource, RunWrite as _, Store, WorkloadWrite as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::run::{Cause, Run, RunEnd, RunStatus};
use uuid::Uuid;

const CANCEL: &str = "run.cancel_run";

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// A run, read under `workload.read` on its workload.
    ///
    /// # Errors
    /// No such run the caller may read.
    pub async fn get_run(&self, caller: &Caller, id: Uuid) -> Result<Run, AppError> {
        let run = self.store.run(id).await?.ok_or(AppError::NotFound { kind: "run" })?;
        self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, run.workload))
            .await?;
        Ok(run)
    }

    /// A workload's runs, newest first, read under `workload.read` on it.
    ///
    /// # Errors
    /// No such workload the caller may read.
    pub async fn list_runs(
        &self,
        caller: &Caller,
        workload: Uuid,
        page: Page,
    ) -> Result<Paged<Run>, AppError> {
        self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, workload)).await?;
        let items = self.store.runs(workload, page).await?;
        let next = page.next(&items, |r| r.id);
        Ok(Paged { items, next })
    }

    /// Cancels an active run under `workload.stop` on its workload; its node stops it when its
    /// assignment no longer names it.
    ///
    /// # Errors
    /// No such run the caller may stop, or one that has ended.
    pub async fn cancel_run(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Run>,
        id: Uuid,
    ) -> Result<Reply<Run>, AppError> {
        if let Some(r) = self.replay(caller, idem, CANCEL).await? {
            return Ok(Reply::Replayed(r));
        }
        let run = self.store.run(id).await?.ok_or(AppError::NotFound { kind: "run" })?;
        let object = ObjectRef::new(Kind::Workload, run.workload);
        let permit = self.authorize(caller, "workload.stop", object).await?;
        let mut tx = self.store.begin().await?;
        // Held as the run maker holds it, so the two take the workload's runs in turn.
        tx.lock_workload(run.workload).await?.ok_or(AppError::NotFound { kind: "workload" })?;
        let at = self.clock.now();
        let end =
            RunEnd { status: RunStatus::Cancelled, cause: Cause::Cancelled, exit_status: None, at };
        if !tx.end_run(id, end).await? {
            return Err(AppError::conflict(
                "run.ended",
                format!("the run is {}", run.status.as_str()),
            ));
        }
        let mut effect = Effect::new(caller.principal.clone(), "run.cancelled", Outcome::Succeeded)
            .after(details([("run", id.to_string().into()), ("cause", "cancelled".into())]))
            .under(permit.grant);
        effect.workload = Some(run.workload);
        tx.append_effect(&effect).await?;
        let cancelled =
            Run { status: end.status, cause: Some(end.cause), ended_at: Some(at), ..run };
        finish(tx, caller, idem, CANCEL, cancelled).await
    }
}
