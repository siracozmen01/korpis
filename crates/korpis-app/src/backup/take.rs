// SPDX-License-Identifier: AGPL-3.0-or-later

//! Taking a backup (§5.1 and §5.5 of docs/06-storage.md): a plan of one step on the volume,
//! authorized on the volume's workload; the job's key and access, granted to the one node that
//! holds the volume while that plan is open; and the workload's backups, read on it.

use super::job::{JobGrant, JobRefusal};
use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::idempotency::{Idempotent, Reply};
use crate::page::{Page, Paged};
use crate::ports::{BackupWrite as _, Clock, RecipeSource, Store, Tx as _};
use crate::workload::volume_plan::{Ask, holder};
use korpis_core::backup::{Backup, Repository};
use korpis_core::extension::{AccessMode, CallOutcome, ProviderType};
use korpis_core::object::{Kind, ObjectRef};
use korpis_core::value::Value;
use korpis_core::workload::Plan;
use korpis_core::workload::plan::VolumeOp;
use korpis_core::workload::plan::compute::BackupStep;
use korpis_core::workload::plan::step::StepAction;
use korpis_core::workload::volume::VolumeKind;
use std::time::Duration;
use uuid::Uuid;

/// How long a job's access lasts: long enough for a large volume over a slow link, and no longer
/// than a target grants (§5.6 of docs/06-storage.md).
const JOB_ACCESS: Duration = Duration::from_secs(12 * 3600);

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

    /// The key and access step `step` of plan `plan` needs, for `node`: only the node holding
    /// the backup's volume, only while the plan is open, and only for a backup still pending.
    pub async fn grant_job_access(
        &self,
        node: Uuid,
        plan: Uuid,
        step: u32,
    ) -> Result<Result<JobGrant, JobRefusal>, AppError> {
        let Some(p) = self.store.plan(plan).await?.filter(|p| !p.status.is_terminal()) else {
            return Ok(Err(JobRefusal::NotAssigned));
        };
        let Some(s) =
            p.steps.iter().find(|s| s.index == step && s.action == StepAction::BackupVolume)
        else {
            return Ok(Err(JobRefusal::NotAJob));
        };
        let id = match s.detail.get("backup") {
            Some(Value::String(b)) => Uuid::parse_str(b)
                .map_err(|_| AppError::Internal("a backup step's backup".into()))?,
            _ => return Ok(Err(JobRefusal::NotAJob)),
        };
        let Some(b) = self.store.backup(id).await?.filter(|b| b.plan == p.id) else {
            return Ok(Err(JobRefusal::NotAJob));
        };
        let holds = self
            .store
            .volume(b.volume)
            .await?
            .is_some_and(|v| v.node == node && v.deleted_at.is_none());
        if !holds {
            return Ok(Err(JobRefusal::NotAssigned));
        }
        if b.status != korpis_core::backup::BackupStatus::Pending {
            return Ok(Err(JobRefusal::Ended));
        }
        let repo = self
            .store
            .repository(b.repository)
            .await?
            .ok_or(AppError::NotFound { kind: "repository" })?;
        let data_key = self.data_key(&repo, b.workload).await?;
        let target = CallTarget {
            organization: repo.organization,
            kind: ProviderType::BackupTarget,
            name: repo.provider.clone(),
        };
        let called = self
            .grant_backup_access(
                &target,
                &repo.workload_prefix(b.workload),
                AccessMode::Write,
                JOB_ACCESS,
            )
            .await?;
        match called.outcome {
            CallOutcome::Access(access) => Ok(Ok(JobGrant { data_key, access })),
            CallOutcome::Failed(f) => Ok(Err(JobRefusal::Store(f.reason))),
            _ => Ok(Err(JobRefusal::Store("provider.malformed".into()))),
        }
    }

    /// `workload`'s data key in `repo`, made at its first backup there.
    async fn data_key(
        &self,
        repo: &Repository,
        workload: Uuid,
    ) -> Result<crate::ports::DataKey, AppError> {
        let vault = self.vault()?;
        let failed = |e: crate::ports::VaultError| AppError::Internal(e.to_string());
        let mut tx = self.store.begin().await?;
        let kek =
            tx.repository_key(repo.id).await?.ok_or(AppError::NotFound { kind: "repository" })?;
        let dek = if let Some(dek) = tx.workload_key(repo.id, workload).await? {
            dek
        } else {
            let made = vault.new_dek(&kek, repo.id, workload).map_err(failed)?;
            tx.keep_workload_key(repo.id, workload, &made, self.clock.now()).await?
        };
        tx.commit().await?;
        vault.open_dek(&kek, &dek, repo.id, workload).map_err(failed)
    }
}
