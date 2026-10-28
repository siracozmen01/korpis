// SPDX-License-Identifier: AGPL-3.0-or-later

//! Reading a workload's migrations, and a person ending one (§8.1 of docs/05-scheduling.md).
//! Before its placement moved a migration is abandoned: the source runs on as it was, and the
//! destination's placement is freed with what it held. After, and until its hold ends, it is
//! rolled back, the workload returned to the source; once the destination reported the workload
//! started, its hold may be ended at once, so the source deletes its copies. A plan a person
//! ended is cancelled.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    Clock, EffectWrite as _, MigrationMoves as _, MigrationWrite as _, RecipeSource, Store,
    WorkloadWrite as _,
};
use korpis_core::audit::{Outcome as Done, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::migration::{Migration, MigrationVolume, Phase};
use std::time::SystemTime;
use uuid::Uuid;

use super::conclude::EndedBy;

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

/// What a person asks of a migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// Abandon it before its placement moved.
    Abandon,
    /// Roll it back after, before its hold ends.
    RollBack,
    /// End its hold now.
    Release,
}

impl Ending {
    const fn operation(self) -> &'static str {
        match self {
            Self::Abandon => ABANDON,
            Self::RollBack => ROLLBACK,
            Self::Release => RELEASE,
        }
    }

    const fn action(self) -> &'static str {
        match self {
            Self::Abandon => "migration.abandon",
            Self::RollBack => "migration.rollback",
            Self::Release => "migration.release",
        }
    }

    /// Whether `m` allows it at `now`, and the refusal where it does not.
    fn allowed(self, m: &Migration, now: SystemTime) -> Result<(), AppError> {
        if let Some((o, _)) = &m.outcome {
            let why = format!("the migration is {}", o.as_str());
            return Err(AppError::conflict("migration.ended", why));
        }
        let moved = m.moved_at.is_some();
        match self {
            Self::Abandon if moved => Err(AppError::conflict(
                "migration.cut_over",
                "the workload has moved; roll the migration back instead",
            )),
            Self::RollBack | Self::Release if !moved => Err(AppError::conflict(
                "migration.not_cut_over",
                "the workload has not moved yet; abandon the migration instead",
            )),
            Self::RollBack if m.hold_until.is_some_and(|h| now >= h) => Err(AppError::conflict(
                "migration.hold_ended",
                "the hold ended, so the source may have deleted its copies",
            )),
            Self::Release if m.phase != Phase::Release => Err(AppError::conflict(
                "migration.not_holding",
                "the destination has not reported the workload started yet",
            )),
            _ => Ok(()),
        }
    }
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
        self.end_by_person(caller, idem, (Ending::Abandon, cmd)).await
    }

    /// Rolls a migration back after its cutover, before its hold ends.
    pub async fn rollback_migration(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, MigrationView>,
        cmd: EndMigration,
    ) -> Result<Reply<MigrationView>, AppError> {
        self.end_by_person(caller, idem, (Ending::RollBack, cmd)).await
    }

    /// Ends a migration's hold now, so the source deletes its copies at once.
    pub async fn release_migration(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, MigrationView>,
        migration: Uuid,
    ) -> Result<Reply<MigrationView>, AppError> {
        let cmd = EndMigration { migration, reason: String::new() };
        self.end_by_person(caller, idem, (Ending::Release, cmd)).await
    }

    /// Does `ending` to the migration `cmd` names, for the caller.
    async fn end_by_person(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, MigrationView>,
        (ending, cmd): (Ending, EndMigration),
    ) -> Result<Reply<MigrationView>, AppError> {
        if let Some(r) = self.replay(caller, idem, ending.operation()).await? {
            return Ok(Reply::Replayed(r));
        }
        let reason = match ending {
            Ending::Release => "",
            Ending::Abandon | Ending::RollBack => reason_of(&cmd.reason)?,
        };
        let ((m, _), grant) = self.migration_to_end(caller, cmd.migration).await?;
        let mut tx = self.store.begin().await?;
        tx.lock_workload(m.workload).await?;
        let m = tx.migration_in(m.id).await?.ok_or(AppError::NotFound { kind: "migration" })?;
        let now = self.clock.now();
        ending.allowed(&m, now)?;
        match ending {
            Ending::Abandon => self.abandon_in(&mut tx, &m, (reason, EndedBy::Person)).await?,
            Ending::RollBack => self.roll_back_in(&mut tx, &m, (reason, EndedBy::Person)).await?,
            Ending::Release => {
                tx.advance_migration(m.id, m.phase, (m.phase_since, Some(now))).await?;
            }
        }
        let mut e = caller.effect(ending.action(), Done::Succeeded).under(grant).after(details([
            ("migration", m.id.to_string().into()),
            ("phase", m.phase.as_str().into()),
            ("reason", reason.into()),
        ]));
        (e.workload, e.plan, e.node) = (Some(m.workload), Some(m.plan), Some(m.destination));
        tx.append_effect(&e).await?;
        let view = tx.migration_in(m.id).await?.ok_or(AppError::NotFound { kind: "migration" })?;
        let volumes = tx.migration_volumes_in(m.id).await?;
        finish(tx, caller, idem, ending.operation(), (view, volumes)).await
    }
}
