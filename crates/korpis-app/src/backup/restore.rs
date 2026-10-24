// SPDX-License-Identifier: AGPL-3.0-or-later

//! Restoring a backup into a volume (§5.4 of docs/06-storage.md): into a new directory of it,
//! which loses nothing and applies at once, or replacing it, which loses what it holds and waits
//! for a person. Either is a plan of the workload holding the volume. Putting a workload's backup
//! back into that workload is `backup.restore` there; putting it into another workload's volume
//! hands its contents to that workload's holders, so it takes `backup.read` on the backup's
//! workload and `workload.file.write` on the other. A block device's files are its guest's to
//! parse, so a path of its backup is restored into a new directory of it by a copy inside a guest
//! from the backup's image, staged on the node, while the workload is stopped (Finding 158 of
//! docs/23-walkthroughs.md); all of it, only by replacing the volume or cloning the backup.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply};
use crate::ports::{Clock, RecipeSource, Store};
use crate::workload::volume_plan::{Ask, holder};
use korpis_core::backup::BackupStatus;
use korpis_core::object::{Kind, ObjectRef};
use korpis_core::workload::Plan;
use korpis_core::workload::plan::VolumeOp;
use korpis_core::workload::plan::compute::RestoreStep;
use korpis_core::workload::spec::IsolationTier;
use korpis_core::workload::volume::VolumeKind;
use uuid::Uuid;

/// What a restore is asked to do.
#[derive(Debug, Clone, Copy)]
pub struct RestoreAsk<'a> {
    /// The backup.
    pub backup: Uuid,
    /// The volume restored into; the backup's own when `None`.
    pub volume: Option<Uuid>,
    /// The path in the backup; all of it when empty.
    pub path: &'a str,
    /// A new directory of the volume to restore into, or `None` to replace the volume.
    pub into: Option<&'a str>,
}

/// That `name` can name a new directory at a volume's root, as the helper checks it again.
fn directory_name(name: &str) -> Result<(), AppError> {
    let ok = !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok { Ok(()) } else { Err(AppError::invalid("into", "a plain directory name")) }
}

/// The filesystem `b`'s image holds, for a path of it restored into a new directory of a block
/// device: one path, copied from the image in a guest, since a block device's restore of all of it
/// is a replacing one.
fn image_of(b: &korpis_core::backup::Backup, path: &str) -> Result<String, AppError> {
    if path.is_empty() {
        let why = "a block device's backup restores one path into a new directory; replace the \
                   volume to restore all of it";
        return Err(AppError::invalid("path", why));
    }
    b.image_filesystem.clone().ok_or_else(|| {
        let why = "the backup says of no image a guest reads: replace the volume, or clone the \
                   backup into a new workload";
        AppError::invalid("into", why)
    })
}

/// That `path` is a relative path in a backup, never leaving it.
fn backup_path(path: &str) -> Result<(), AppError> {
    let bad = path.starts_with('/')
        || path.split('/').any(|c| c == ".." || c == "." || (c.is_empty() && !path.is_empty()))
        || path.contains('\0');
    if bad { Err(AppError::invalid("path", "a relative path in the backup")) } else { Ok(()) }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Proposes restoring as `ask` says.
    pub async fn restore_backup(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        ask: RestoreAsk<'_>,
    ) -> Result<Reply<Plan>, AppError> {
        match ask.into {
            Some(into) => directory_name(into)?,
            None if !ask.path.is_empty() => {
                return Err(AppError::invalid("path", "a replacing restore restores all of it"));
            }
            None => {}
        }
        backup_path(ask.path)?;
        let b = self.store.backup(ask.backup).await?.filter(|b| b.deleted_at.is_none());
        let b = b.ok_or(AppError::NotFound { kind: "backup" })?;
        if b.status != BackupStatus::Succeeded {
            return Err(AppError::conflict("backup.not_whole", "only a whole backup restores"));
        }
        let v = self.live_volume(ask.volume.unwrap_or(b.volume)).await?;
        if v.kind != VolumeKind::Declared {
            return Err(AppError::invalid(
                "volume",
                "a console stream's volume is not restored into",
            ));
        }
        let target = holder(&v);
        let image_filesystem = match ask.into {
            Some(_) if self.block_devices(target).await? => Some(image_of(&b, ask.path)?),
            _ => None,
        };
        let source = ObjectRef::new(Kind::Workload, b.workload);
        let action = if target == b.workload {
            "backup.restore"
        } else {
            self.authorize(caller, "backup.read", source).await?;
            "workload.file.write"
        };
        let restore = RestoreStep {
            job: Uuid::now_v7(),
            backup: b.id,
            path: ask.path,
            into: ask.into,
            image_filesystem: image_filesystem.as_deref(),
        };
        let request = Ask {
            volume: v,
            op: VolumeOp::Restore,
            action,
            object: ObjectRef::new(Kind::Workload, target),
            snapshot: None,
            backup: None,
            restore: Some(restore),
        };
        self.propose_on_volume(caller, idem, request).await
    }

    /// Whether `workload`'s volumes are block devices, as they are on a tier that runs a guest
    /// kernel (§3.1 of docs/06-storage.md): its approved intent's, or its head's before one is.
    async fn block_devices(&self, workload: Uuid) -> Result<bool, AppError> {
        let Some(w) = self.store.workload(workload).await? else { return Ok(false) };
        let intent = self.store.intent(w.target.unwrap_or(w.intent)).await?;
        Ok(intent.is_some_and(|i| i.body.runtime.tier >= IsolationTier::MicroVm))
    }
}
