// SPDX-License-Identifier: AGPL-3.0-or-later

//! The steps of plans that move a volume's data: a backup taken of it, and a restore into it.

use super::{Builder, deletion, text};
use crate::object::{Kind, ObjectRef};
use crate::value::{Value, ValueMap};
use crate::workload::plan::step::{Classifier, Step, StepAction};
use crate::workload::volume::Volume;
use uuid::Uuid;

/// A backup a plan takes: its identifier, its repository, the parent whose unchanged files it
/// need not read, and whether the node reads it from a snapshot it takes for the job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupStep {
    /// The backup.
    pub backup: Uuid,
    /// The repository it is kept in.
    pub repository: Uuid,
    /// The volume's last whole backup in that repository.
    pub parent: Option<Uuid>,
    /// Whether the class can snapshot, so the job reads a single point in time.
    pub consistent: bool,
}

/// The one step of a plan backing up `v`: the node snapshots it for the job where `consistent`,
/// reads it, and releases the snapshot, all inside the step (§5.1 of docs/06-storage.md), so
/// taking a backup changes nothing a workload sees and loses nothing.
#[must_use]
pub fn backup_steps(
    workload: Uuid,
    v: &Volume,
    b: BackupStep,
    classifier: Classifier<'_>,
) -> Vec<Step> {
    let mut steps =
        Builder { classifier, target: ObjectRef::new(Kind::Workload, workload), steps: Vec::new() };
    let mut detail = deletion(v);
    detail.insert("backup".into(), text(&b.backup.to_string()));
    detail.insert("repository".into(), text(&b.repository.to_string()));
    detail.insert("parent".into(), text(&b.parent.map(|p| p.to_string()).unwrap_or_default()));
    detail.insert("consistent".into(), Value::Bool(b.consistent));
    steps.push(StepAction::BackupVolume, detail, &[]);
    steps.steps
}

/// A restore a plan makes into a volume (§5.4 of docs/06-storage.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreStep<'a> {
    /// The job that does it, named once so every step of the plan names the same one.
    pub job: Uuid,
    /// The backup restored.
    pub backup: Uuid,
    /// The path in it restored; empty for all of it.
    pub path: &'a str,
    /// The new directory of the volume restored into, or `None` to replace the volume.
    pub into: Option<&'a str>,
    /// For a block device, the filesystem its backup's image holds: a path is then copied from
    /// the image inside a guest (§5.4 of docs/06-storage.md).
    pub image_filesystem: Option<&'a str>,
}

/// The steps of a plan restoring into `v`: into a new directory of it, one step that loses
/// nothing; or replacing it, the backup restored into a staging place while the workload runs,
/// then the workload stopped when it runs (`running`), the volume replaced, and the workload
/// started again, so it is down only for the exchange. A path of a block device's backup goes
/// into a new directory the same way: its image staged while the workload runs, then the path
/// copied from it in a guest while the workload is stopped, since a block device is attached to
/// one guest at a time.
#[must_use]
pub fn restore_steps(
    workload: Uuid,
    v: &Volume,
    r: RestoreStep<'_>,
    running: bool,
    classifier: Classifier<'_>,
) -> Vec<Step> {
    let mut b =
        Builder { classifier, target: ObjectRef::new(Kind::Workload, workload), steps: Vec::new() };
    let mut detail = deletion(v);
    detail.insert("job".into(), text(&r.job.to_string()));
    detail.insert("backup".into(), text(&r.backup.to_string()));
    detail.insert("path".into(), text(r.path));
    detail.insert("into".into(), text(r.into.unwrap_or_default()));
    detail.insert("replace".into(), Value::Bool(r.into.is_none()));
    if let Some(fs) = r.image_filesystem {
        detail.insert("image_filesystem".into(), text(fs));
    }
    let applied = match (r.into, r.image_filesystem) {
        (Some(_), None) => {
            b.push(StepAction::RestoreVolume, detail, &[]);
            return b.steps;
        }
        (Some(_), Some(_)) => StepAction::ApplyRestore,
        (None, _) => StepAction::ReplaceVolume,
    };
    b.push(StepAction::RestoreVolume, detail.clone(), &[]);
    if running {
        b.push(StepAction::Stop, ValueMap::new(), &[]);
    }
    b.push(applied, detail, &[]);
    if running {
        b.push(StepAction::Start, ValueMap::new(), &[]);
    }
    b.steps
}
