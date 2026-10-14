// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deleting a retained volume no workload holds (§7 of docs/06-storage.md): a plan of one
//! irreversible step, which always waits for a person. The plan belongs to the workload that
//! created the volume, whose identifier is kept forever, retired or not; its node executes it,
//! through a residual assignment when the workload no longer runs there (§4.8 of
//! docs/02-architecture.md).

use super::propose::{self, Proposal};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{Clock, EffectWrite, RecipeSource, Store, WorkloadWrite};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::plan::compute::delete_volume_steps;
use korpis_core::workload::plan::impact::ResourceDelta;
use korpis_core::workload::volume::{Volume, VolumeKind};
use korpis_core::workload::{Plan, PlanTarget};
use uuid::Uuid;

const OPERATION: &str = "volume.delete_volume";

/// Why `v` cannot be deleted on its own, if it cannot: it is gone, or a workload holds it, which
/// is deleted with that workload or detached from it first.
pub(crate) fn undeletable(v: &Volume) -> Option<AppError> {
    if v.deleted_at.is_some() {
        return Some(AppError::conflict("volume.deleted", "the volume is already deleted"));
    }
    v.attached_to.map(|w| {
        let why = format!("the volume is attached to {w}: delete it with that workload");
        AppError::conflict("volume.attached", why)
    })
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Proposes deleting the retained volume `id`, under `volume.delete` on it.
    pub async fn delete_volume(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        id: Uuid,
    ) -> Result<Reply<Plan>, AppError> {
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let object = ObjectRef::new(Kind::Volume, id);
        let (permit, proposing) =
            self.authorize_declaration(caller, "volume.delete", object).await?;
        let v = self.store.volume(id).await?.ok_or(AppError::NotFound { kind: "volume" })?;
        if let Some(e) = undeletable(&v) {
            return Err(e);
        }
        let open = self.store.open_plans(v.created_by).await?;
        if open.iter().any(|p| p.target == PlanTarget::DeleteVolume(id)) {
            return Err(AppError::conflict("volume.deleting", "a plan deleting it is open"));
        }
        let w =
            self.store.workload(v.created_by).await?.ok_or_else(|| {
                AppError::Internal(format!("volume {id} names a missing workload"))
            })?;
        let (driver, disruption) = self.running_layers(&w).await?;
        let classifier = propose::classifier(&driver, &disruption, &w);
        let gone = if v.kind == VolumeKind::Declared { -1 } else { 0 };
        let proposal = Proposal {
            workload: w.id,
            target: PlanTarget::DeleteVolume(id),
            from_intent: w.target,
            from_power: w.power_target_version,
            delta: ResourceDelta { volumes: gone, ..ResourceDelta::default() },
            steps: delete_volume_steps(w.id, &v, classifier),
            proposing: &proposing,
            grant: permit.grant,
        };
        let plan = propose::plan(caller, proposal, self.clock.now());
        let mut tx = self.store.begin().await?;
        tx.insert_plan(&plan).await?;
        let mut e = Effect::new(caller.principal.clone(), "volume.delete", Outcome::Succeeded)
            .under(permit.grant)
            .after(details([
                ("volume", id.to_string().into()),
                ("approval", propose::approval_name(&plan).into()),
            ]));
        e.workload = Some(w.id);
        e.plan = Some(plan.id);
        tx.append_effect(&e).await?;
        finish(tx, caller, idem, OPERATION, plan).await
    }
}
