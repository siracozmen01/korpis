// SPDX-License-Identifier: AGPL-3.0-or-later

//! Beginning an approved migration (§8.1 of docs/05-scheduling.md). Approving the plan makes the
//! migration record, in `prepare`, and binds the destination, under the placement lock, as a
//! second placement of the workload that is not authoritative, so its capacity, cores, and
//! devices are held from then on. The destination is checked again as it was proposed, since
//! the node may have filled meanwhile; one that no longer fits refuses the approval, and nothing
//! was done. The plan applies from then until the migration ends.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    Clock, EffectWrite as _, MigrationWrite as _, PlacementWrite as _, RecipeSource, Store,
    WorkloadWrite as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::placement::{BoundBy, Placement};
use korpis_core::principal::{Principal, SystemActor};
use korpis_core::workload::migration::{Migration, Phase};
use korpis_core::workload::plan::PlanStatus;
use korpis_core::workload::plan::compute::destination_of;
use korpis_core::workload::{Plan, Workload};
use std::time::SystemTime;
use uuid::Uuid;

use super::choose::Chosen;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Begins the migration `plan`, approved now, carries out for `w`: its record, and the
    /// destination's placement. The plan becomes applying.
    pub(in crate::workload) async fn begin_migration(
        &self,
        tx: &mut S::Tx,
        plan: &mut Plan,
        w: &Workload,
    ) -> Result<(), AppError> {
        if plan.status != PlanStatus::Approved {
            let why = "another plan of the workload applies; approve this one once it is done";
            return Err(AppError::conflict("plan.applying", why));
        }
        if tx.live_migration_in(w.id).await?.is_some() {
            return Err(super::migrating());
        }
        let (node, hold) = destination_of(&plan.steps).ok_or_else(|| {
            AppError::Internal(format!("migration plan {} names no node", plan.id))
        })?;
        let target = self.approved_target(w).await?;
        tx.lock_placement().await?;
        let chosen = self.destination(tx, (w, &target), Some(node)).await?;
        let volumes = self.crossings(w, (&chosen.source, &chosen.node)).await?;
        let now = self.clock.now();
        let source = chosen.source.id;
        tx.bind(&destination_placement((w.id, target.id), chosen, now)).await?;
        let m = Migration {
            id: Uuid::now_v7(),
            plan: plan.id,
            workload: w.id,
            source,
            destination: node,
            phase: Phase::Prepare,
            phase_since: now,
            hold,
            hold_until: None,
            outcome: None,
            made_at: now,
        };
        tx.insert_migration(&m, &volumes).await?;
        plan.transition(PlanStatus::Applying).map_err(|e| AppError::Internal(e.to_string()))?;
        if !tx.update_plan(plan, PlanStatus::Approved).await? {
            return Err(AppError::conflict("plan.superseded", "the plan changed meanwhile"));
        }
        let mut e = begun(&m, volumes.len());
        e.grant = tx.decision_grant(w.id).await?;
        tx.append_effect(&e).await?;
        Ok(())
    }
}

/// The placement of `workload`, approved to be `intent`, on the destination `chosen` names: not
/// authoritative until the cutover.
fn destination_placement(
    (workload, intent): (Uuid, Uuid),
    chosen: Chosen,
    now: SystemTime,
) -> Placement {
    Placement {
        workload,
        node: chosen.node.id,
        intent,
        reserved: chosen.given.reserved,
        bound_at: now,
        bound_by: BoundBy::Migration,
        reason: "a migration's destination, until its cutover".to_owned(),
        explanation: chosen.explanation,
        authoritative: false,
        held_at: None,
        devices: chosen.given.devices,
        cores: chosen.given.cores,
    }
}

/// The effect of beginning `m`, which moves `volumes` volumes.
fn begun(m: &Migration, volumes: usize) -> Effect {
    let mut e = Effect::new(
        Principal::System(SystemActor::Scheduler),
        "migration.begin",
        Outcome::Succeeded,
    )
    .after(details([
        ("migration", m.id.to_string().into()),
        ("source", m.source.to_string().into()),
        ("destination", m.destination.to_string().into()),
        ("volumes", volumes.into()),
    ]));
    (e.workload, e.plan, e.node) = (Some(m.workload), Some(m.plan), Some(m.destination));
    e
}
