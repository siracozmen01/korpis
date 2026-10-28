// SPDX-License-Identifier: AGPL-3.0-or-later

//! How a migration ends (§8.1 of docs/05-scheduling.md), whoever ends it. Abandoned, the source
//! runs on and the destination's placement is freed with what it held. Rolled back, the
//! placement moves back to the source where it had moved, each volume onto the copy the source
//! kept, and what the destination's copies hold since is kept as a divergent volume of each, as
//! a failover onto a replica leaves one (§2.2 of docs/06-storage.md). Completed, the source's
//! placement is freed once it deleted its copies. Its plan ends with it: applied where it
//! completed, failed where a phase failed, and cancelled where a person ended it.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, MigrationMoves as _, MigrationWrite as _, RecipeSource, Store, WorkloadWrite as _,
};
use korpis_core::workload::migration::{Migration, Outcome};
use korpis_core::workload::plan::PlanStatus;
use uuid::Uuid;

/// Who ends a migration that does not complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::workload) enum EndedBy {
    /// A side reported a phase failed: the plan failed.
    Failure,
    /// A person: the plan is cancelled.
    Person,
}

impl EndedBy {
    const fn plan_status(self) -> PlanStatus {
        match self {
            Self::Failure => PlanStatus::Failed,
            Self::Person => PlanStatus::Cancelled,
        }
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Ends the plan `id`, applying, as `to`; `completed` its one step where it applied.
    async fn end_plan(
        &self,
        tx: &mut S::Tx,
        id: Uuid,
        to: PlanStatus,
        completed: Option<u32>,
    ) -> Result<(), AppError> {
        let Some(mut plan) = tx.lock_plan(id).await? else { return Ok(()) };
        let from = plan.status;
        if plan.transition(to).is_ok() {
            plan.completed = completed.or(plan.completed);
            tx.update_plan(&plan, from).await?;
        }
        Ok(())
    }

    /// Abandons `m`, before its placement moved, for `why`, as `by` ends it.
    pub(in crate::workload) async fn abandon_in(
        &self,
        tx: &mut S::Tx,
        m: &Migration,
        (why, by): (&str, EndedBy),
    ) -> Result<(), AppError> {
        tx.end_migration(m.id, (Outcome::Abandoned, why), self.clock.now()).await?;
        tx.free_placement(m.workload, m.destination).await?;
        self.end_plan(tx, m.plan, by.plan_status(), None).await
    }

    /// Rolls `m` back for `why`, as `by` ends it.
    pub(in crate::workload) async fn roll_back_in(
        &self,
        tx: &mut S::Tx,
        m: &Migration,
        (why, by): (&str, EndedBy),
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        if let Some(moved_at) = m.moved_at {
            let volumes = tx.migration_volumes_in(m.id).await?;
            let back: Vec<(Uuid, String)> =
                volumes.iter().map(|v| (v.volume, v.source_helper_id.clone())).collect();
            tx.move_placement((m.id, m.workload), (m.destination, m.source), &back, now).await?;
            let copies = volumes.iter().filter(|v| v.crossing.sends()).filter_map(|v| {
                v.destination_helper_id.as_deref().map(|helper| (v.volume, helper))
            });
            for (volume, helper) in copies {
                tx.keep_divergent(volume, (m.destination, helper), (now, moved_at)).await?;
            }
            if let Some(w) = tx.lock_workload(m.workload).await? {
                self.count_placed(tx, &w).await?;
            }
        }
        tx.end_migration(m.id, (Outcome::RolledBack, why), now).await?;
        tx.free_placement(m.workload, m.destination).await?;
        self.end_plan(tx, m.plan, by.plan_status(), None).await
    }

    /// Completes `m`, the source having deleted its copies.
    pub(in crate::workload) async fn complete_migration(
        &self,
        tx: &mut S::Tx,
        m: &Migration,
    ) -> Result<(), AppError> {
        let why = "the workload runs on the destination, and the source's copies are deleted";
        tx.end_migration(m.id, (Outcome::Completed, why), self.clock.now()).await?;
        tx.free_placement(m.workload, m.source).await?;
        self.end_plan(tx, m.plan, PlanStatus::Applied, Some(0)).await
    }
}
