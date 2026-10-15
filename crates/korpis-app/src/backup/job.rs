// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node's backup or restore job is given, or why it is given nothing (§5.5 of
//! docs/06-storage.md): the workload's data key and access to its prefix in the repository,
//! granted to the one node holding the volume the step operates on, while its plan is open. A
//! backup writes, and its workload's key is made at its first backup; a restore reads, with the
//! key of the workload whose backup it is.

use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{
    BackupWrite as _, Clock, DataKey, PruneWrite as _, RecipeSource, Store, Tx as _,
};
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

/// How much later than its own clock says the control plane takes access to end: the target's
/// clock decides, and the two may disagree by this much.
pub(super) const CLOCK_SKEW: Duration = Duration::from_secs(5 * 60);

/// A key and access to a workload's backups: a job's, or the control plane's own when it reads
/// one.
#[derive(Debug)]
pub struct JobGrant {
    /// The repository it reaches.
    pub repository: Uuid,
    /// The workload whose key and prefix it is.
    pub workload: Uuid,
    /// The workload's data key, opened for this job.
    pub data_key: super::keys::JobKey,
    /// Access to the workload's prefix, until it expires.
    pub access: ObjectAccess,
    /// Access to the repository's recipe content, in the same mode, for as long; `None` where
    /// the target refused it or the job needs none, and the job does without (§5.2 of
    /// docs/06-storage.md).
    pub content: Option<ObjectAccess>,
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
    /// No repository takes the workload's streams.
    NoTarget,
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
            Self::NoTarget => "offload.none".into(),
        }
    }
}

/// Where a job reads or writes: a workload's prefix in a repository, and the backup it reads.
struct JobTarget {
    repo: Repository,
    workload: Uuid,
    mode: AccessMode,
    backup: Option<Uuid>,
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
    /// `recipient` is the one-time public key the job made, which a key under `tenant` custody
    /// is sealed to.
    pub async fn grant_job_access(
        &self,
        node: Uuid,
        (plan, step): (Uuid, u32),
        recipient: &[u8],
    ) -> Judged<JobGrant> {
        let Some(p) = self.store.plan(plan).await?.filter(|p| !p.status.is_terminal()) else {
            return Ok(Err(JobRefusal::NotAssigned));
        };
        let Some(s) = p.steps.iter().find(|s| s.index == step) else {
            return Ok(Err(JobRefusal::NotAJob));
        };
        let judged = match s.action {
            StepAction::BackupVolume => self.backup_job(node, &p, s).await?,
            StepAction::RestoreVolume => self.restore_job(node, &p, s).await?,
            _ => Err(JobRefusal::NotAJob),
        };
        let t = match judged {
            Ok(t) => t,
            Err(refused) => return Ok(Err(refused)),
        };
        let make = t.mode == AccessMode::Write;
        let data_key = self.job_key((&t.repo, t.workload), make, recipient).await?;
        let target = CallTarget {
            organization: t.repo.organization,
            kind: ProviderType::BackupTarget,
            name: t.repo.provider.clone(),
        };
        let prefix = ((t.repo.id, t.workload), crate::ports::Area::Backups);
        self.record_access(prefix, (t.backup, Some(node)), t.mode, JOB_ACCESS).await?;
        let prefix = t.repo.workload_prefix(t.workload);
        let called = self.grant_backup_access(&target, &prefix, t.mode, JOB_ACCESS).await?;
        match called.outcome {
            CallOutcome::Access(access) => {
                let content = self.content_access(&target, &t.repo, t.mode).await?;
                let (repository, workload) = (t.repo.id, t.workload);
                Ok(Ok(JobGrant { repository, workload, data_key, access, content }))
            }
            CallOutcome::Failed(f) => Ok(Err(JobRefusal::Store(f.reason))),
            _ => Ok(Err(JobRefusal::Store("provider.malformed".into()))),
        }
    }

    /// Access to `repo`'s recipe content in `mode`: `None` where the target refused it, since a
    /// backup still records content by reference, and a restore still reads it from the node.
    async fn content_access(
        &self,
        target: &CallTarget,
        repo: &Repository,
        mode: AccessMode,
    ) -> Result<Option<ObjectAccess>, AppError> {
        let prefix = repo.content_prefix();
        let called = self.grant_backup_access(target, &prefix, mode, JOB_ACCESS).await?;
        Ok(match called.outcome {
            CallOutcome::Access(access) => Some(access),
            _ => None,
        })
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
        Ok(Ok(JobTarget { repo, workload: b.workload, mode: AccessMode::Write, backup: None }))
    }

    /// A restore step's target: a whole backup, restored into a volume `node` holds, or into
    /// one the plan creates on `node`, where its workload is placed.
    async fn restore_job(&self, node: Uuid, p: &Plan, s: &Step) -> Judged<JobTarget> {
        let Some(id) = uuid_in(s, "backup") else { return Ok(Err(JobRefusal::NotAJob)) };
        let assigned = match uuid_in(s, "volume_id") {
            Some(volume) => self.holds(node, Some(volume)).await?,
            None => self.store.placement(p.workload).await?.is_some_and(|at| at.node == node),
        };
        if !assigned {
            return Ok(Err(JobRefusal::NotAssigned));
        }
        let whole = |b: &korpis_core::backup::Backup| {
            b.status == BackupStatus::Succeeded && b.deleted_at.is_none()
        };
        let Some(b) = self.store.backup(id).await?.filter(whole) else {
            return Ok(Err(JobRefusal::Gone));
        };
        let repo = self.repository_of(b.repository).await?;
        Ok(Ok(JobTarget { repo, workload: b.workload, mode: AccessMode::Read, backup: Some(b.id) }))
    }

    /// Records access to `prefix` for `ttl` from now, before it is asked for, so a sweep knows
    /// of every writer and a prune of every reader (§5.5 of docs/06-storage.md). It is recorded
    /// as lasting a little longer than asked, since the target's clock decides when it ends.
    pub(super) async fn record_access(
        &self,
        (prefix, area): (crate::ports::Prefix, crate::ports::Area),
        (backup, node): (Option<Uuid>, Option<Uuid>),
        mode: AccessMode,
        ttl: Duration,
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        let access = crate::ports::AccessRecord {
            prefix,
            area,
            backup,
            mode,
            node,
            granted_at: now,
            expires_at: now + ttl + CLOCK_SKEW,
        };
        let mut tx = self.store.begin().await?;
        tx.record_access(&access).await?;
        tx.commit().await?;
        Ok(())
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
