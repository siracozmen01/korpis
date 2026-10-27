// SPDX-License-Identifier: AGPL-3.0-or-later

//! Plans on a volume, apart from any change to a workload (§4 and §7 of docs/06-storage.md):
//! deleting a retained volume, and taking, rolling back to, and deleting its snapshots. Each is a
//! plan of the workload holding the volume, or of the one that created it when none holds it,
//! whose identifier is kept forever, retired or not; the volume's node executes it, through that
//! workload's assignment when it runs there and through a residual one otherwise (§4.8 of
//! docs/02-architecture.md). One plan at a time operates on a volume.

use super::propose::{self, Proposal, approve_if_auto};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::ports::{
    BackupWrite as _, Clock, EffectWrite, PruneWrite as _, RecipeSource, Store, WorkloadWrite,
};
use korpis_core::audit::{Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::backup::{Backup, BackupStats, BackupStatus};
use korpis_core::node::driver::StorageFeature;
use korpis_core::workload::plan::VolumeOp;
use korpis_core::workload::plan::compute::{
    BackupStep, RestoreStep, backup_steps, delete_volume_steps, restore_steps, snapshot_steps,
};
use korpis_core::workload::plan::impact::ResourceDelta;
use korpis_core::workload::volume::{Volume, VolumeKind};
use korpis_core::workload::{Plan, PlanTarget, PowerState};
use uuid::Uuid;

/// The workload whose plans operate on `v`: the one holding it, or the one that created it.
pub(crate) const fn holder(v: &Volume) -> Uuid {
    match v.attached_to {
        Some(w) => w,
        None => v.created_by,
    }
}

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

/// The record of backup `b`, pending, as its plan `plan` of `workload` on `volume` proposes it.
fn pending(
    b: BackupStep,
    (workload, volume, plan): (Uuid, Uuid, Uuid),
    now: std::time::SystemTime,
) -> Backup {
    Backup {
        id: b.backup,
        repository: b.repository,
        workload,
        volume,
        plan,
        parent: b.parent,
        status: BackupStatus::Pending,
        consistent: b.consistent,
        stats: BackupStats::default(),
        error: None,
        created: now,
        finished: None,
        deleted_at: None,
        image_filesystem: None,
    }
}

/// What the effect of proposing `ask` as `plan` records.
fn proposed(ask: &Ask<'_>, plan: &Plan) -> serde_json::Map<String, serde_json::Value> {
    let mut after = details([
        ("volume", ask.volume.id.to_string().into()),
        ("approval", propose::approval_name(plan).into()),
    ]);
    if let Some(b) = ask.backup {
        after.insert("backup".into(), b.backup.to_string().into());
        after.insert("repository".into(), b.repository.to_string().into());
    }
    if let Some(r) = ask.restore {
        after.insert("backup".into(), r.backup.to_string().into());
        after.insert("replace".into(), r.into.is_none().into());
    }
    if let Some((id, name)) = ask.snapshot {
        after.insert("snapshot".into(), id.to_string().into());
        after.insert("name".into(), name.into());
    }
    after
}

/// A plan asked of a volume.
pub(crate) struct Ask<'a> {
    pub volume: Volume,
    pub op: VolumeOp,
    /// The action it is proposed under, and the object that action is authorized on: the
    /// volume, or for a backup or a restore the volume's workload.
    pub action: &'static str,
    pub object: ObjectRef,
    /// The snapshot it concerns, by identifier and name.
    pub snapshot: Option<(Uuid, &'a str)>,
    /// The backup it takes.
    pub backup: Option<BackupStep>,
    /// The restore it makes.
    pub restore: Option<RestoreStep<'a>>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Proposes deleting the retained volume `id`, under `volume.delete` on it.
    pub async fn delete_volume(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        id: Uuid,
    ) -> Result<Reply<Plan>, AppError> {
        let volume = self.store.volume(id).await?.ok_or(AppError::NotFound { kind: "volume" })?;
        if let Some(e) = undeletable(&volume) {
            return Err(e);
        }
        let object = ObjectRef::new(Kind::Volume, volume.id);
        let ask = Ask {
            volume,
            op: VolumeOp::Delete,
            action: "volume.delete",
            object,
            snapshot: None,
            backup: None,
            restore: None,
        };
        self.propose_on_volume(caller, idem, ask).await
    }

    /// Proposes taking the snapshot `name` of volume `id`, under `volume.snapshot.create`, in a
    /// class that can.
    pub async fn create_snapshot(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        (id, name): (Uuid, &str),
    ) -> Result<Reply<Plan>, AppError> {
        let name = korpis_core::tenancy::Name::parse(name)
            .map_err(|_| AppError::invalid("name", "a lowercase name"))?;
        let volume = self.live_volume(id).await?;
        if volume.kind != VolumeKind::Declared {
            return Err(AppError::invalid("volume", "a console stream's volume is not snapshot"));
        }
        self.can_snapshot(&volume).await?;
        let taken = self.store.snapshots(id, false).await?;
        if taken.iter().any(|s| s.name == name.as_str()) {
            return Err(AppError::conflict(
                "snapshot.exists",
                "the volume has a snapshot so named",
            ));
        }
        let snapshot = Some((Uuid::now_v7(), name.as_str()));
        let object = ObjectRef::new(Kind::Volume, volume.id);
        let (op, action) = (VolumeOp::Snapshot, "volume.snapshot.create");
        let ask = Ask { volume, op, action, object, snapshot, backup: None, restore: None };
        self.propose_on_volume(caller, idem, ask).await
    }

    /// Proposes rolling the volume of snapshot `id` back to it, under
    /// `volume.snapshot.rollback`, or deleting the snapshot, under `volume.snapshot.delete`.
    pub async fn on_snapshot(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        (id, op): (Uuid, VolumeOp),
    ) -> Result<Reply<Plan>, AppError> {
        let not_found = AppError::NotFound { kind: "snapshot" };
        let s = self.store.snapshot(id).await?.filter(|s| s.deleted_at.is_none());
        let s = s.ok_or(not_found)?;
        let volume = self.live_volume(s.volume).await?;
        let action = match op {
            VolumeOp::Rollback => "volume.snapshot.rollback",
            VolumeOp::DeleteSnapshot => "volume.snapshot.delete",
            VolumeOp::Delete | VolumeOp::Snapshot | VolumeOp::Backup | VolumeOp::Restore => {
                return Err(AppError::Internal("not an operation on a snapshot".into()));
            }
        };
        let object = ObjectRef::new(Kind::Volume, volume.id);
        let snapshot = Some((s.id, s.name.as_str()));
        let ask = Ask { volume, op, action, object, snapshot, backup: None, restore: None };
        self.propose_on_volume(caller, idem, ask).await
    }

    /// Whether `plan`, on volume `id`, can still be approved: the volume is live, still held by
    /// the plan's workload or by none, and, for a deletion, held by none.
    pub(crate) async fn volume_plan_current(
        &self,
        plan: &Plan,
        (id, op): (Uuid, VolumeOp),
    ) -> Result<bool, AppError> {
        let Some(v) = self.store.volume(id).await?.filter(|v| v.deleted_at.is_none()) else {
            return Ok(false);
        };
        let deletable = op != VolumeOp::Delete || undeletable(&v).is_none();
        Ok(holder(&v) == plan.workload && deletable)
    }

    pub(crate) async fn live_volume(&self, id: Uuid) -> Result<Volume, AppError> {
        let v = self.store.volume(id).await?.filter(|v| v.deleted_at.is_none());
        v.ok_or(AppError::NotFound { kind: "volume" })
    }

    /// That `v`'s class, on its node, declares snapshots, or why not.
    pub(crate) async fn can_snapshot(&self, v: &Volume) -> Result<(), AppError> {
        let node = self.store.node(v.keeper()).await?;
        let class = node.as_ref().and_then(|n| {
            n.storage_classes
                .iter()
                .find(|c| v.storage_class.is_empty() || c.name == v.storage_class)
        });
        match class {
            Some(c) if c.capabilities.has(StorageFeature::Snapshot) => Ok(()),
            Some(c) => Err(AppError::conflict(
                "volume.cannot_snapshot",
                format!("storage class {:?} ({:?}) cannot snapshot", c.name, c.backend),
            )),
            None => Err(AppError::conflict(
                "volume.cannot_snapshot",
                "the volume's node no longer declares its storage class",
            )),
        }
    }

    /// The steps of `ask`, a plan of `w` on its volume.
    async fn volume_steps(
        &self,
        w: &korpis_core::workload::Workload,
        ask: &Ask<'_>,
        classifier: korpis_core::workload::plan::step::Classifier<'_>,
    ) -> Result<Vec<korpis_core::workload::plan::Step>, AppError> {
        let v = &ask.volume;
        Ok(match (ask.backup, ask.restore, ask.snapshot) {
            (Some(b), _, _) => backup_steps(w.id, v, b, classifier),
            (None, Some(r), _) => restore_steps(w.id, v, r, self.runs(w, v).await?, classifier),
            (None, None, None) => delete_volume_steps(w.id, v, classifier),
            (None, None, Some(snapshot)) => {
                let running = self.runs(w, v).await?;
                snapshot_steps(w.id, (v, ask.op), snapshot, running, classifier)
            }
        })
    }

    /// Proposes `ask` as a plan of the volume's holder, approved at once if nothing in it needs
    /// a person, which taking a snapshot or a backup can be.
    pub(crate) async fn propose_on_volume(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        ask: Ask<'_>,
    ) -> Result<Reply<Plan>, AppError> {
        let operation = format!("volume.{}", ask.op.kind());
        if let Some(r) = self.replay(caller, idem, &operation).await? {
            return Ok(Reply::Replayed(r));
        }
        let v = &ask.volume;
        let (permit, proposing) =
            self.authorize_declaration(caller, ask.action, ask.object).await?;
        let w = self.store.workload(holder(v)).await?.ok_or_else(|| {
            AppError::Internal(format!("volume {} names a missing workload", v.id))
        })?;
        if self.store.open_plans(w.id).await?.iter().any(|p| p.target.volume() == Some(v.id)) {
            return Err(AppError::conflict("volume.busy", "a plan operating on it is open"));
        }
        let (driver, disruption, wait) = self.running_layers(&w).await?;
        let classifier = propose::classifier(&driver, (&disruption, wait), &w);
        let steps = self.volume_steps(&w, &ask, classifier).await?;
        let gone = ask.op == VolumeOp::Delete && v.kind == VolumeKind::Declared;
        let proposal = Proposal {
            exposures: Vec::new(),
            workload: w.id,
            target: PlanTarget::Volume(v.id, ask.op),
            from_intent: w.target,
            from_power: w.power_target_version,
            delta: ResourceDelta { volumes: if gone { -1 } else { 0 }, ..ResourceDelta::default() },
            steps,
            proposing: &proposing,
            grant: permit.grant,
        };
        let mut plan = propose::plan(caller, proposal, self.clock.now());
        let mut tx = self.store.begin().await?;
        tx.insert_plan(&plan).await?;
        if let Some(b) = ask.backup {
            if tx.swept((b.repository, w.id)).await? {
                let why = "a prune is deleting what the workload's backups no longer reference";
                return Err(AppError::conflict("backup.busy", why));
            }
            tx.insert_backup(&pending(b, (w.id, v.id, plan.id), self.clock.now())).await?;
        }
        if approve_if_auto(&mut tx, &mut plan, caller, self.clock.now()).await? {
            tx.update_plan(&plan, korpis_core::workload::plan::PlanStatus::Proposed).await?;
        }
        let mut e = caller
            .effect(ask.action, Outcome::Succeeded)
            .under(permit.grant)
            .after(proposed(&ask, &plan));
        e.workload = Some(w.id);
        e.plan = Some(plan.id);
        tx.append_effect(&e).await?;
        finish(tx, caller, idem, &operation, plan).await
    }

    /// Whether `w` runs with `v` attached, so that a rollback stops and starts it.
    async fn runs(
        &self,
        w: &korpis_core::workload::Workload,
        v: &Volume,
    ) -> Result<bool, AppError> {
        if w.deleted_at.is_some() || v.attached_to != Some(w.id) {
            return Ok(false);
        }
        let power = self.store.power_at(w.id, w.power_target_version).await?;
        Ok(power.is_some_and(|p| p.power.desired == PowerState::Running))
    }
}
