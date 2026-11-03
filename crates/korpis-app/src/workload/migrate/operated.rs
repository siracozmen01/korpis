// SPDX-License-Identifier: AGPL-3.0-or-later

//! A migration an operation generates: proposed toward the scheduler's choice and approved in one
//! transaction, by the person who approved the operation and under the grant they approved it
//! under, since that person decided "drain this node" and not each move again. The plan names the
//! operation, and is computed as the workload is now, never ahead of its turn.

use super::Moving;
use super::choose::Toward;
use super::{changed_addresses, crossing};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{
    Clock, EffectWrite as _, MigrationWrite as _, RecipeSource, Store, WorkloadWrite as _,
};
use korpis_core::audit::{Outcome, details};
use korpis_core::node::snapshot::restores;
use korpis_core::workload::migration::DEFAULT_HOLD;
use korpis_core::workload::plan::{PlanStatus, Proposing};
use korpis_core::workload::{Plan, Workload};
use uuid::Uuid;

/// Who an operation's migration is carried out for: the operation, its approver, and the grant
/// they approved it under.
pub(crate) type For<'a> = (Uuid, &'a Caller, Uuid);

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Moves `w`, locked in `tx` and placed on `node`, off it for the operation `operation`: its
    /// plan proposed, approved, and begun. A workload migrating already, or with a plan applying,
    /// is refused, and so is one no other node takes.
    pub(crate) async fn migrate_for(
        &self,
        tx: &mut S::Tx,
        (operation, approver, grant): For<'_>,
        w: &Workload,
    ) -> Result<Plan, AppError> {
        self.migratable(w).await?;
        if tx.live_migration_in(w.id).await?.is_some() {
            return Err(super::migrating());
        }
        if tx.plan_applying(w.id).await? {
            let why = "another plan of the workload applies; it moves once that is done";
            return Err(AppError::conflict("plan.applying", why));
        }
        let target = self.approved_target(w).await?;
        let chosen = self.destination(tx, (w, &target), Toward::Any).await?;
        let volumes = self.crossings(w, (&chosen.source, &chosen.node)).await?;
        let moving = Moving {
            source: chosen.source.id,
            destination: chosen.node.id,
            named: false,
            hold: DEFAULT_HOLD,
            volumes: volumes.iter().map(crossing::crossed).collect(),
            live: restores(&chosen.source, &chosen.node, &target.body.runtime.driver),
            changed_addresses: changed_addresses(&target, chosen.edge),
        };
        let mut plan =
            self.migrate_plan(approver, w, (&Proposing::Permitted, grant), &moving).await?;
        plan.operation = Some(operation);
        plan.transition(PlanStatus::Approved).map_err(|e| AppError::Internal(e.to_string()))?;
        plan.approved_by = Some(approver.principal.clone());
        plan.approved_at = Some(self.clock.now());
        tx.insert_plan(&plan).await?;
        let mut e =
            approver.effect("plan.approve", Outcome::Succeeded).under(grant).after(details([
                ("operation", operation.to_string().into()),
                ("source", moving.source.to_string().into()),
                ("destination", moving.destination.to_string().into()),
            ]));
        (e.workload, e.plan) = (Some(w.id), Some(plan.id));
        tx.append_effect(&e).await?;
        self.begin_migration(tx, &mut plan, w).await?;
        Ok(plan)
    }
}
