// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node's backup or restore job is given, or why it is given nothing (§5.5 of
//! docs/06-storage.md): the workload's data key and access to its prefix in the repository,
//! granted to the one node holding the volume the step operates on, while its plan is open. A
//! backup writes, and its workload's key is made at its first backup; a restore reads, with the
//! key of the workload whose backup it is.

use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{BackupWrite as _, Clock, DataKey, RecipeSource, Store, Tx as _};
use korpis_core::backup::{BackupStatus, Repository};
use korpis_core::extension::{AccessMode, CallOutcome, ObjectAccess, ProviderType};
use korpis_core::value::Value;
use korpis_core::workload::Plan;
use korpis_core::workload::plan::Step;
use korpis_core::workload::plan::step::StepAction;
use std::time::Duration;
use uuid::Uuid;

/// How long a job's access lasts: long enough for a large volume over a slow link, and no longer
/// than a target grants (§5.6 of docs/06-storage.md).
const JOB_ACCESS: Duration = Duration::from_secs(12 * 3600);

/// A key and access to a workload's backups: a job's, or the control plane's own when it reads
/// one.
#[derive(Debug)]
pub struct JobGrant {
    /// The workload's data key, opened for this job.
    pub data_key: DataKey,
    /// Access to the workload's prefix, until it expires.
    pub access: ObjectAccess,
}

/// Why a job is given nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobRefusal {
    /// The plan is not open, or the node does not hold the volume it operates on.
    NotAssigned,
    /// The step is not a backup or restore.
    NotAJob,
    /// The backup taken already ended.
    Ended,
    /// The backup restored is no longer whole: it failed, or was pruned.
    Gone,
    /// The backup target did not grant access, for the provider's reason.
    Store(String),
}

impl JobRefusal {
    /// The stable reason the node is told.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::NotAssigned => "job.not_assigned".into(),
            Self::NotAJob => "job.not_a_job".into(),
            Self::Ended => "job.ended".into(),
            Self::Gone => "job.backup_gone".into(),
            Self::Store(reason) => reason.clone(),
        }
    }
}

/// Where a job reads or writes: a workload's prefix in a repository.
struct JobTarget {
    repo: Repository,
    workload: Uuid,
    mode: AccessMode,
}

fn uuid_in(s: &Step, key: &str) -> Option<Uuid> {
    match s.detail.get(key) {
        Some(Value::String(v)) => Uuid::parse_str(v).ok(),
        _ => None,
    }
}

type Judged<T> = Result<Result<T, JobRefusal>, AppError>;

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The key and access step `step` of plan `plan` needs, for `node`.
    pub async fn grant_job_access(&self, node: Uuid, plan: Uuid, step: u32) -> Judged<JobGrant> {
        let Some(p) = self.store.plan(plan).await?.filter(|p| !p.status.is_terminal()) else {
            return Ok(Err(JobRefusal::NotAssigned));
        };
        let Some(s) = p.steps.iter().find(|s| s.index == step) else {
            return Ok(Err(JobRefusal::NotAJob));
        };
        let judged = match s.action {
            StepAction::BackupVolume => self.backup_job(node, &p, s).await?,
            StepAction::RestoreVolume => self.restore_job(node, s).await?,
            _ => Err(JobRefusal::NotAJob),
        };
        let t = match judged {
            Ok(t) => t,
            Err(refused) => return Ok(Err(refused)),
        };
        let data_key = self.data_key(&t.repo, t.workload, t.mode == AccessMode::Write).await?;
        let target = CallTarget {
            organization: t.repo.organization,
            kind: ProviderType::BackupTarget,
            name: t.repo.provider.clone(),
        };
        let prefix = t.repo.workload_prefix(t.workload);
        let called = self.grant_backup_access(&target, &prefix, t.mode, JOB_ACCESS).await?;
        match called.outcome {
            CallOutcome::Access(access) => Ok(Ok(JobGrant { data_key, access })),
            CallOutcome::Failed(f) => Ok(Err(JobRefusal::Store(f.reason))),
            _ => Ok(Err(JobRefusal::Store("provider.malformed".into()))),
        }
    }

    /// Whether `node` holds the live volume `volume`.
    async fn holds(&self, node: Uuid, volume: Option<Uuid>) -> Result<bool, AppError> {
        let Some(volume) = volume else { return Ok(false) };
        let v = self.store.volume(volume).await?;
        Ok(v.is_some_and(|v| v.node == node && v.deleted_at.is_none()))
    }

    async fn repository_of(&self, id: Uuid) -> Result<Repository, AppError> {
        let repo = self.store.repository(id).await?;
        repo.ok_or(AppError::NotFound { kind: "repository" })
    }

    /// A backup step's target: the pending backup of plan `p`, on a volume `node` holds.
    async fn backup_job(&self, node: Uuid, p: &Plan, s: &Step) -> Judged<JobTarget> {
        let Some(id) = uuid_in(s, "backup") else { return Ok(Err(JobRefusal::NotAJob)) };
        let Some(b) = self.store.backup(id).await?.filter(|b| b.plan == p.id) else {
            return Ok(Err(JobRefusal::NotAJob));
        };
        if !self.holds(node, Some(b.volume)).await? {
            return Ok(Err(JobRefusal::NotAssigned));
        }
        if b.status != BackupStatus::Pending {
            return Ok(Err(JobRefusal::Ended));
        }
        let repo = self.repository_of(b.repository).await?;
        Ok(Ok(JobTarget { repo, workload: b.workload, mode: AccessMode::Write }))
    }

    /// A restore step's target: a whole backup, restored into a volume `node` holds.
    async fn restore_job(&self, node: Uuid, s: &Step) -> Judged<JobTarget> {
        let Some(id) = uuid_in(s, "backup") else { return Ok(Err(JobRefusal::NotAJob)) };
        if !self.holds(node, uuid_in(s, "volume_id")).await? {
            return Ok(Err(JobRefusal::NotAssigned));
        }
        let whole = |b: &korpis_core::backup::Backup| {
            b.status == BackupStatus::Succeeded && b.deleted_at.is_none()
        };
        let Some(b) = self.store.backup(id).await?.filter(whole) else {
            return Ok(Err(JobRefusal::Gone));
        };
        let repo = self.repository_of(b.repository).await?;
        Ok(Ok(JobTarget { repo, workload: b.workload, mode: AccessMode::Read }))
    }

    /// `workload`'s data key in `repo`; made at its first backup there when `make`, and
    /// otherwise required to exist.
    pub(super) async fn data_key(
        &self,
        repo: &Repository,
        workload: Uuid,
        make: bool,
    ) -> Result<DataKey, AppError> {
        let vault = self.vault()?;
        let failed = |e: crate::ports::VaultError| AppError::Internal(e.to_string());
        let mut tx = self.store.begin().await?;
        let kek =
            tx.repository_key(repo.id).await?.ok_or(AppError::NotFound { kind: "repository" })?;
        let dek = match tx.workload_key(repo.id, workload).await? {
            Some(dek) => dek,
            None if make => {
                let sealed = vault.new_dek(&kek, repo.id, workload).map_err(failed)?;
                tx.keep_workload_key(repo.id, workload, &sealed, self.clock.now()).await?
            }
            None => return Err(AppError::Internal("a backup's workload has no key".into())),
        };
        tx.commit().await?;
        vault.open_dek(&kek, &dek, repo.id, workload).map_err(failed)
    }
}
