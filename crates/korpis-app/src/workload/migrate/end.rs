// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a workload's migrations, and ending one (§8.1 of docs/05-scheduling.md). Before
//! `cutover` a migration is abandoned: the source runs on as it was, the destination's placement
//! is freed with what it held, and its plan is cancelled. After `cutover` and until its hold ends
//! it is rolled back, the workload returned to the source, or released, the source's copies
//! deleted at once; each needs the placement to have moved, which only a migration past
//! `verify` has done.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    Clock, EffectWrite as _, MigrationWrite as _, RecipeSource, Store, WorkloadWrite as _,
};
use korpis_core::audit::{Outcome as Done, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::migration::{Migration, MigrationVolume, Outcome, Phase};
use korpis_core::workload::plan::PlanStatus;
use uuid::Uuid;

const ABANDON: &str = "workload.abandon_migration";
const ROLLBACK: &str = "workload.rollback_migration";
const RELEASE: &str = "workload.release_migration";

/// The longest reason, in characters.
const MAX_REASON: usize = 500;

/// A migration with its volumes, as it is read.
pub type MigrationView = (Migration, Vec<MigrationVolume>);

/// A migration to end, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndMigration {
    /// The migration.
    pub migration: Uuid,
    /// Why, for the record; required to abandon or roll back.
    pub reason: String,
}

/// The refusal of what only a migration past cutover may do.
fn not_cut_over() -> AppError {
    let why = "the workload has not moved yet; abandon the migration instead";
    AppError::conflict("migration.not_cut_over", why)
}

/// The refusal of anything asked of a migration that ended.
fn ended(m: &Migration) -> AppError {
    let how = m.outcome.as_ref().map_or("ended", |(o, _)| o.as_str());
    AppError::conflict("migration.ended", format!("the migration is {how}"))
}

/// `reason`, trimmed, if it says something in at most [`MAX_REASON`] characters.
fn reason_of(reason: &str) -> Result<&str, AppError> {
    let r = reason.trim();
    if r.is_empty() || r.chars().count() > MAX_REASON {
        return Err(AppError::invalid("reason", "why, in at most 500 characters"));
    }
    Ok(r)
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The migrations of a workload the caller may read, newest first.
    pub async fn migrations(
        &self,
        caller: &Caller,
        workload: Uuid,
    ) -> Result<Vec<MigrationView>, AppError> {
        self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, workload)).await?;
        Ok(self.store.migrations_of(workload).await?)
    }

    /// The migration `id` and its volumes, the caller holding `workload.migrate` on its workload.
    async fn migration_to_end(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<(MigrationView, Uuid), AppError> {
        let found = self.store.migration(id).await?.ok_or(AppError::NotFound { kind: "migration" });
        let (m, volumes) = found?;
        let object = ObjectRef::new(Kind::Workload, m.workload);
        let permit = self.authorize(caller, "workload.migrate", object).await?;
        Ok(((m, volumes), permit.grant))
    }

    /// Abandons a migration before its cutover.
    pub async fn abandon_migration(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, MigrationView>,
        cmd: EndMigration,
    ) -> Result<Reply<MigrationView>, AppError> {
        if let Some(r) = self.replay(caller, idem, ABANDON).await? {
            return Ok(Reply::Replayed(r));
        }
        let reason = reason_of(&cmd.reason)?;
        let ((m, volumes), grant) = self.migration_to_end(caller, cmd.migration).await?;
        let mut tx = self.store.begin().await?;
        tx.lock_workload(m.workload).await?;
        let m = tx.migration_in(m.id).await?.ok_or(AppError::NotFound { kind: "migration" })?;
        if !m.live() {
            return Err(ended(&m));
        }
        if m.phase.moved() {
            let why = "the workload has moved; roll the migration back instead";
            return Err(AppError::conflict("migration.cut_over", why));
        }
        let now = self.clock.now();
        tx.end_migration(m.id, (Outcome::Abandoned, reason), now).await?;
        tx.unbind_destination(m.workload, m.destination).await?;
        if let Some(mut plan) = tx.lock_plan(m.plan).await? {
            let from = plan.status;
            if plan.transition(PlanStatus::Cancelled).is_ok() {
                tx.update_plan(&plan, from).await?;
            }
        }
        let mut e =
            caller.effect("migration.abandon", Done::Succeeded).under(grant).after(details([
                ("migration", m.id.to_string().into()),
                ("phase", m.phase.as_str().into()),
                ("reason", reason.into()),
            ]));
        (e.workload, e.plan, e.node) = (Some(m.workload), Some(m.plan), Some(m.destination));
        tx.append_effect(&e).await?;
        let mut done = m;
        done.outcome = Some((Outcome::Abandoned, reason.to_owned()));
        finish(tx, caller, idem, ABANDON, (done, volumes)).await
    }

    /// Rolls a migration back after its cutover, before its hold ends.
    pub async fn rollback_migration(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, MigrationView>,
        cmd: EndMigration,
    ) -> Result<Reply<MigrationView>, AppError> {
        if let Some(r) = self.replay(caller, idem, ROLLBACK).await? {
            return Ok(Reply::Replayed(r));
        }
        reason_of(&cmd.reason)?;
        self.after_cutover(caller, cmd.migration).await
    }

    /// Ends a migration's hold now, the source's copies deleted.
    pub async fn release_migration(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, MigrationView>,
        migration: Uuid,
    ) -> Result<Reply<MigrationView>, AppError> {
        if let Some(r) = self.replay(caller, idem, RELEASE).await? {
            return Ok(Reply::Replayed(r));
        }
        self.after_cutover(caller, migration).await
    }

    /// What may be asked only of a migration past its cutover: refused before it, and, until
    /// moving a workload is built through its cutover, refused after.
    async fn after_cutover(
        &self,
        caller: &Caller,
        migration: Uuid,
    ) -> Result<Reply<MigrationView>, AppError> {
        let ((m, _), _) = self.migration_to_end(caller, migration).await?;
        if !m.live() {
            return Err(ended(&m));
        }
        if m.phase < Phase::Cutover {
            return Err(not_cut_over());
        }
        Err(AppError::NotYet { what: "a migration past its cutover", phase: 3 })
    }
}
