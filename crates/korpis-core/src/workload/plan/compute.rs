// SPDX-License-Identifier: AGPL-3.0-or-later

//! Computing a plan's steps: the difference between what the workload was approved to be and
//! what is now declared, as actions a node and the control plane can execute.

use super::VolumeOp;
use super::step::{Classifier, Step, StepAction, StepExecutor};
use crate::object::{Kind, ObjectRef};
use crate::recipe::RecipeError;
use crate::recipe::document::Recipe;
use crate::value::{Value, ValueMap};
use crate::workload::spec::{IntentBody, VolumeSpec};
use crate::workload::volume::{Volume, VolumeKind};
use crate::workload::{Power, PowerState};
use uuid::Uuid;

mod data;

pub use data::{BackupStep, RestoreStep, backup_steps, restore_steps};

struct Builder<'a> {
    classifier: Classifier<'a>,
    target: ObjectRef,
    steps: Vec<Step>,
}

fn int(n: u64) -> Value {
    Value::Int(i64::try_from(n).unwrap_or(i64::MAX))
}

fn text(s: &str) -> Value {
    Value::String(s.to_owned())
}

impl Builder<'_> {
    fn push(&mut self, action: StepAction, detail: ValueMap, touched: &[String]) {
        let (disruptive, classified_by, reason) = self.classifier.classify(action, touched);
        let (_, reversible) = action.floor();
        self.steps.push(Step {
            index: u32::try_from(self.steps.len()).unwrap_or(u32::MAX),
            action,
            executor: StepExecutor::Node,
            target: self.target,
            reversible,
            disruptive,
            classified_by,
            reason,
            detail,
            timeout: None,
        });
    }

    fn pull(&mut self, body: &IntentBody) {
        let detail = ValueMap::from([("digest".to_owned(), text(&body.recipe.digest))]);
        self.push(StepAction::PullRecipe, detail, &[]);
    }

    fn add_volume(&mut self, v: &VolumeSpec) {
        let mut detail = ValueMap::from([("volume".to_owned(), text(&v.name))]);
        if let Some(id) = v.existing {
            detail.insert("existing".into(), text(&id.to_string()));
            self.push(StepAction::AttachVolume, detail, &[]);
        } else {
            detail.insert("size_bytes".into(), int(v.size_bytes));
            detail.insert("storage_class".into(), text(&v.storage_class));
            self.push(StepAction::CreateVolume, detail, &[]);
            if let Some(backup) = v.from_backup {
                self.seed(&v.name, backup);
            }
        }
    }

    /// Restores `backup` into the volume `name` just created, before anything runs on it; its
    /// node knows it by the name, as it made it.
    fn seed(&mut self, name: &str, backup: Uuid) {
        let detail = ValueMap::from([
            ("volume".to_owned(), text(name)),
            ("job".to_owned(), text(&Uuid::now_v7().to_string())),
            ("backup".to_owned(), text(&backup.to_string())),
            ("path".to_owned(), text("")),
            ("into".to_owned(), text("")),
            ("replace".to_owned(), Value::Bool(false)),
            ("seed".to_owned(), Value::Bool(true)),
        ]);
        self.push(StepAction::RestoreVolume, detail, &[]);
    }
}

/// Configuration keys whose values differ.
fn changed_keys(a: &ValueMap, b: &ValueMap) -> Vec<String> {
    a.keys()
        .chain(b.keys())
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether moving from `a` to `b` needs the running instance restarted: anything the process is
/// started with changed.
fn needs_restart(a: &IntentBody, b: &IntentBody) -> bool {
    a.recipe.digest != b.recipe.digest
        || a.runtime != b.runtime
        || a.resources != b.resources
        || a.volumes != b.volumes
        || a.config != b.config
        || a.lifecycle != b.lifecycle
        || a.console != b.console
}

fn first_intent(
    b: &mut Builder<'_>,
    to: &IntentBody,
    power: PowerState,
    install: Option<ValueMap>,
) {
    b.pull(to);
    for v in &to.volumes {
        b.add_volume(v);
    }
    // Into the volumes just made, before anything runs on them. Written only into volumes this
    // plan creates, it writes over nothing, and undoing the plan removes what it wrote: then, and
    // only then, an install can be undone (§4 of docs/09-recipes.md).
    if let Some(detail) = install {
        b.push(StepAction::Install, detail, &[]);
        if let Some(step) = b.steps.last_mut() {
            step.reversible = to.volumes.iter().all(|v| v.existing.is_none());
        }
    }
    b.push(StepAction::Create, ValueMap::new(), &[]);
    if power == PowerState::Running {
        b.push(StepAction::Start, ValueMap::new(), &[]);
    }
}

fn changed_intent(
    b: &mut Builder<'_>,
    (from, to): (&IntentBody, &IntentBody),
    power: PowerState,
    install: Option<ValueMap>,
) {
    if from.recipe.digest != to.recipe.digest {
        b.pull(to);
    }
    for v in to.volumes.iter().filter(|v| !from.volumes.iter().any(|o| o.name == v.name)) {
        b.add_volume(v);
    }
    for v in from.volumes.iter().filter(|v| !to.volumes.iter().any(|n| n.name == v.name)) {
        let detail = ValueMap::from([("volume".to_owned(), text(&v.name))]);
        b.push(StepAction::DetachVolume, detail, &[]);
    }
    let touched = changed_keys(&from.config, &to.config);
    let restart = needs_restart(from, to);
    // An install writes what the running instance has open: it runs stopped, and the instance
    // is started again after it rather than restarted.
    let installing = install.is_some();
    let running = power == PowerState::Running;
    if installing && running {
        b.push(StepAction::Stop, ValueMap::new(), &[]);
    }
    if let Some(detail) = install {
        b.push(StepAction::Install, detail, &touched);
    }
    if restart
        || from.health != to.health
        || from.placement != to.placement
        || from.continuity != to.continuity
    {
        let fields = Value::List(touched.iter().map(|k| text(k)).collect());
        b.push(
            StepAction::Reconfigure,
            ValueMap::from([("config_keys".to_owned(), fields)]),
            &touched,
        );
    }
    if from.endpoints != to.endpoints {
        b.push(StepAction::ReconfigureNetwork, ValueMap::new(), &[]);
    }
    if installing && running {
        b.push(StepAction::Start, ValueMap::new(), &[]);
    } else if restart && running {
        b.push(StepAction::Restart, ValueMap::new(), &[]);
    }
}

/// The steps of an intent plan for `workload`: from the approved body (`None` for a new
/// workload) to the declared one, whose recipe is `recipe` when it is known, given whether the
/// workload is to be running. Identical bodies give no steps.
///
/// # Errors
/// The install a step carries cannot be built: a template's context is missing a value.
pub fn intent_steps(
    workload: Uuid,
    (from, to): (Option<&IntentBody>, &IntentBody),
    recipe: Option<&Recipe>,
    power: PowerState,
    classifier: Classifier<'_>,
) -> Result<Vec<Step>, RecipeError> {
    let mut b =
        Builder { classifier, target: ObjectRef::new(Kind::Workload, workload), steps: Vec::new() };
    let install = match recipe {
        Some(r) => super::install::install_detail(from, to, r)?,
        None => None,
    };
    match from {
        None => first_intent(&mut b, to, power, install),
        Some(from) => changed_intent(&mut b, (from, to), power, install),
    }
    Ok(b.steps)
}

/// The steps of a power plan for `workload`: a start, a stop, a restart, or nothing.
#[must_use]
pub fn power_steps(
    workload: Uuid,
    from: Power,
    to: Power,
    classifier: Classifier<'_>,
) -> Vec<Step> {
    let mut b =
        Builder { classifier, target: ObjectRef::new(Kind::Workload, workload), steps: Vec::new() };
    let detail = ValueMap::from([("cycle".to_owned(), int(to.cycle))]);
    match (from.desired, to.desired) {
        (PowerState::Stopped, PowerState::Running) => b.push(StepAction::Start, detail, &[]),
        (PowerState::Running, PowerState::Stopped) => b.push(StepAction::Stop, detail, &[]),
        (PowerState::Running, PowerState::Running) if to.cycle > from.cycle => {
            b.push(StepAction::Restart, detail, &[]);
        }
        _ => {}
    }
    b.steps
}

/// The steps of a retire plan for `workload` (§4.8 of docs/02-architecture.md), from its
/// approved body and power. A workload stops, its volumes are detached and retained, and its
/// runtime is removed; then each volume in `deleting`, which the deletion named explicitly, is
/// deleted by its own irreversible step (§7 of docs/06-storage.md), after the runtime that
/// mounted it is gone. A workload nothing was ever approved for has reached no node, holds no
/// volume, and the control plane retires it alone.
#[must_use]
pub fn retire_steps(
    workload: Uuid,
    (target, power): (Option<&IntentBody>, PowerState),
    deleting: &[Volume],
    classifier: Classifier<'_>,
) -> Vec<Step> {
    let mut b =
        Builder { classifier, target: ObjectRef::new(Kind::Workload, workload), steps: Vec::new() };
    let Some(body) = target else {
        b.push(StepAction::Retire, ValueMap::new(), &[]);
        if let Some(step) = b.steps.last_mut() {
            step.executor = StepExecutor::ControlPlane;
        }
        return b.steps;
    };
    if power == PowerState::Running {
        b.push(StepAction::Stop, ValueMap::new(), &[]);
    }
    let deleted = |name: &str| deleting.iter().any(|v| v.name == name);
    let kept: Vec<&str> =
        body.volumes.iter().map(|v| v.name.as_str()).filter(|n| !deleted(n)).collect();
    for name in &kept {
        let detail = ValueMap::from([
            ("volume".to_owned(), text(name)),
            ("retained".to_owned(), Value::Bool(true)),
        ]);
        b.push(StepAction::DetachVolume, detail, &[]);
    }
    let retained = Value::List(kept.iter().map(|n| text(n)).collect());
    let gone = Value::List(deleting.iter().map(|v| text(&v.name)).collect());
    let detail = ValueMap::from([
        ("volumes_retained".to_owned(), retained),
        ("volumes_deleted".to_owned(), gone),
    ]);
    b.push(StepAction::Retire, detail, &[]);
    for v in deleting {
        b.push(StepAction::DeleteVolume, deletion(v), &[]);
    }
    b.steps
}

/// The one step of a plan deleting the retained volume `v`, executed by its node.
#[must_use]
pub fn delete_volume_steps(workload: Uuid, v: &Volume, classifier: Classifier<'_>) -> Vec<Step> {
    let mut b =
        Builder { classifier, target: ObjectRef::new(Kind::Workload, workload), steps: Vec::new() };
    b.push(StepAction::DeleteVolume, deletion(v), &[]);
    b.steps
}

/// The steps of a plan on volume `v` for the snapshot `snapshot` (its identifier and name): take
/// it; roll the volume back to it, stopping and starting the holding workload around it when it
/// runs (`running`), because a rollback replaces what the workload has open; or delete it.
#[must_use]
pub fn snapshot_steps(
    workload: Uuid,
    (v, op): (&Volume, VolumeOp),
    (snapshot, name): (Uuid, &str),
    running: bool,
    classifier: Classifier<'_>,
) -> Vec<Step> {
    let mut b =
        Builder { classifier, target: ObjectRef::new(Kind::Workload, workload), steps: Vec::new() };
    let mut detail = deletion(v);
    detail.insert("snapshot_id".into(), text(&snapshot.to_string()));
    detail.insert("snapshot".into(), text(name));
    match op {
        VolumeOp::Snapshot => b.push(StepAction::SnapshotVolume, detail, &[]),
        VolumeOp::Rollback if running => {
            b.push(StepAction::Stop, ValueMap::new(), &[]);
            b.push(StepAction::RollbackVolume, detail, &[]);
            b.push(StepAction::Start, ValueMap::new(), &[]);
        }
        VolumeOp::Rollback => b.push(StepAction::RollbackVolume, detail, &[]),
        VolumeOp::DeleteSnapshot => b.push(StepAction::DeleteSnapshot, detail, &[]),
        VolumeOp::Delete => b.push(StepAction::DeleteVolume, deletion(v), &[]),
        // A backup's or a restore's steps are `backup_steps`'s or `restore_steps`'s, with what
        // only they carry.
        VolumeOp::Backup | VolumeOp::Restore => {}
    }
    b.steps
}

/// What a node needs to delete `v`: the helper's identifier for it, as the node reported it,
/// never one derived again from its name.
fn deletion(v: &Volume) -> ValueMap {
    ValueMap::from([
        ("volume".to_owned(), text(&v.name)),
        ("volume_id".to_owned(), text(&v.id.to_string())),
        ("helper_id".to_owned(), text(&v.helper_id)),
        ("stream".to_owned(), Value::Bool(v.kind == VolumeKind::Stream)),
    ])
}
