// SPDX-License-Identifier: AGPL-3.0-or-later

//! Taking a backup (§5.1 of docs/06-storage.md): a plan of one step on the volume, authorized
//! on the volume's workload; and the workload's backups, read on it.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply};
use crate::page::{Page, Paged};
use crate::ports::{Clock, RecipeSource, Store};
use crate::workload::volume_plan::{Ask, holder};
use korpis_core::backup::Backup;
use korpis_core::object::{Kind, ObjectRef};
use korpis_core::workload::Plan;
use korpis_core::workload::plan::VolumeOp;
use korpis_core::workload::plan::compute::BackupStep;
use korpis_core::workload::volume::VolumeKind;
use uuid::Uuid;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Proposes backing volume `volume` up into `repository`, under `backup.create` on the
    /// volume's workload and `repository.read` on the repository.
    pub async fn create_backup(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Plan>,
        (volume, repository): (Uuid, Uuid),
    ) -> Result<Reply<Plan>, AppError> {
        let v = self.live_volume(volume).await?;
        if v.kind != VolumeKind::Declared {
            return Err(AppError::invalid(
                "volume",
                "a console stream's volume is offloaded, not backed up",
            ));
        }
        let w = self
            .store
            .workload(holder(&v))
            .await?
            .ok_or(AppError::NotFound { kind: "workload" })?;
        self.authorize(caller, "repository.read", ObjectRef::new(Kind::Repository, repository))
            .await?;
        let repo = self.store.repository(repository).await?.filter(|r| r.deleted_at.is_none());
        let repo = repo.ok_or(AppError::NotFound { kind: "repository" })?;
        if repo.organization != w.organization || repo.project.is_some_and(|p| p != w.project) {
            return Err(AppError::invalid(
                "repository",
                "a repository serving the workload's project",
            ));
        }
        if self.store.backup_open(repo.id, w.id).await? {
            return Err(AppError::conflict(
                "backup.busy",
                "a backup of the workload into the repository is open",
            ));
        }
        let backup = BackupStep {
            backup: Uuid::now_v7(),
            repository: repo.id,
            parent: self.store.last_backup(repo.id, v.id).await?,
            consistent: self.can_snapshot(&v).await.is_ok(),
        };
        let object = ObjectRef::new(Kind::Workload, w.id);
        let ask = Ask {
            volume: v,
            op: VolumeOp::Backup,
            action: "backup.create",
            object,
            snapshot: None,
            backup: Some(backup),
            restore: None,
        };
        self.propose_on_volume(caller, idem, ask).await
    }

    /// One backup's record, read under `workload.read` on its workload: that a backup exists
    /// and what it holds is the workload's state, while its file tree and contents are the
    /// private `backup.browse` and `backup.read` (§3.3 and §7 of docs/08-identity.md).
    pub async fn get_backup(&self, caller: &Caller, id: Uuid) -> Result<Backup, AppError> {
        let b = self.store.backup(id).await?.ok_or(AppError::NotFound { kind: "backup" })?;
        self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, b.workload)).await?;
        Ok(b)
    }

    /// A workload's backup records, newest first, read under `workload.read` on it.
    pub async fn list_backups(
        &self,
        caller: &Caller,
        (workload, deleted): (Uuid, bool),
        page: Page,
    ) -> Result<Paged<Backup>, AppError> {
        self.authorize(caller, "workload.read", ObjectRef::new(Kind::Workload, workload)).await?;
        let items = self.store.backups(workload, deleted, page).await?;
        let next = page.next(&items, |b| b.id);
        Ok(Paged { items, next })
    }
}
