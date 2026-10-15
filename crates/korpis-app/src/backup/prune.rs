// SPDX-License-Identifier: AGPL-3.0-or-later

//! The first phase of a prune (§5.5 of docs/06-storage.md), the step of an approved prune plan:
//! the backups the plan names are forgotten, except one being read now, and every object beneath
//! the workload's prefix that no other backup references is marked. What references what is read
//! by the control plane itself, with read access and the workload's key, never taken from a node:
//! a host that could name what to delete could delete its own history. The sweep that deletes
//! what was marked is queued for when a grace period longer than any job's access has passed.

use super::job::JobGrant;
use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{
    Area, Clock, Job, JobWrite as _, NewJob, Prefix, PruneWrite as _, RecipeSource, Store, Tx as _,
};
use korpis_core::audit::{Outcome, details};
use korpis_core::backup::Repository;
use korpis_core::extension::{AccessMode, MAX_ACCESS, ProviderType, StoredObject};
use korpis_core::workload::Plan;
use korpis_core::workload::plan::Step;
use korpis_core::workload::plan::prune::forgotten;
use std::collections::BTreeSet;
use std::time::Duration;
use uuid::Uuid;

/// The kind of job a sweep is.
pub const SWEEP: &str = "backup_sweep";

/// How long marked objects wait to be deleted: longer than any job's access, so a writer that
/// listed them before they were marked has lost its access before they go.
pub const GRACE: Duration = Duration::from_secs(MAX_ACCESS.as_secs() + 3600);

/// How long the control plane's own read access lasts for one phase of a prune.
const PRUNE_ACCESS: Duration = Duration::from_secs(3600);

/// How many attempts a sweep is given.
const SWEEP_ATTEMPTS: u32 = 20;

/// What a phase of a prune reads: a workload's prefix in a repository, every object there, and
/// which backups are no longer kept.
#[derive(Debug)]
pub struct PruneRead {
    /// The repository and the workload.
    pub prefix: Prefix,
    /// The workload's place in the target, ending in `/`.
    pub place: String,
    /// Every object beneath it, by its name beneath it.
    pub objects: Vec<StoredObject>,
    /// The backups forgotten, this prune's included: their records are not kept.
    pub forgotten: BTreeSet<Uuid>,
    /// Read access to the place and the workload's key; `None` when the workload has no key in
    /// the repository, so nothing is there to read.
    pub grant: Option<JobGrant>,
}

impl PruneRead {
    /// The backups whose records are beneath the prefix and not forgotten: what is kept.
    #[must_use]
    pub fn kept(&self) -> Vec<String> {
        let records = self.objects.iter().filter_map(|o| o.key.strip_prefix("backups/"));
        let live = |id: &&str| Uuid::parse_str(id).is_ok_and(|u| !self.forgotten.contains(&u));
        records.filter(live).map(str::to_owned).collect()
    }
}

/// What the step of a prune plan reads, and the backups it forgets.
#[derive(Debug)]
pub struct PruneWork {
    /// The plan and the step.
    pub step: (Uuid, Step),
    /// The backups it forgets: those the plan names that no one is reading now.
    pub forget: Vec<Uuid>,
    /// What it reads.
    pub read: PruneRead,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// What the prune step `step` of `plan` reads.
    pub async fn begin_prune(&self, (plan, step): (Plan, Step)) -> Result<PruneWork, AppError> {
        let repository =
            plan.target.repository().ok_or(AppError::NotFound { kind: "repository" })?;
        let repo = self.store.repository(repository).await?;
        let repo = repo.ok_or(AppError::NotFound { kind: "repository" })?;
        let prefix = (repo.id, plan.workload);
        let read = self.store.backups_being_read(prefix, self.clock.now()).await?;
        let forget: Vec<Uuid> =
            forgotten(&step).into_iter().filter(|b| !read.contains(b)).collect();
        let read = self.prune_read(&repo, plan.workload, &forget).await?;
        Ok(PruneWork { step: (plan.id, step), forget, read })
    }

    /// What a phase of a prune of `workload` in `repo` reads, with `forget` about to be forgotten.
    pub(super) async fn prune_read(
        &self,
        repo: &Repository,
        workload: Uuid,
        forget: &[Uuid],
    ) -> Result<PruneRead, AppError> {
        let prefix = (repo.id, workload);
        let mut forgotten = self.store.forgotten_backups(prefix).await?;
        forgotten.extend(forget);
        let target = CallTarget {
            organization: repo.organization,
            kind: ProviderType::BackupTarget,
            name: repo.provider.clone(),
        };
        let place = repo.workload_prefix(workload);
        let objects = self.list_all_objects(&target, &place).await?;
        let grant = if self.has_key(prefix).await? {
            Some(self.control_plane_access((repo, Area::Backups), workload, PRUNE_ACCESS).await?)
        } else {
            None
        };
        Ok(PruneRead { prefix, place, objects, forgotten, grant })
    }

    /// Ends prune job `job` of `worker`: `work`'s backups forgotten, everything beneath the
    /// prefix that `referenced` does not name marked, the sweep queued, the step done.
    pub async fn finish_prune(
        &self,
        (job, worker): (&Job, &str),
        work: &PruneWork,
        referenced: &BTreeSet<String>,
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        let (plan, step) = &work.step;
        let garbage: Vec<String> = work
            .read
            .objects
            .iter()
            .map(|o| o.key.clone())
            .filter(|k| !referenced.contains(k))
            .collect();
        let mut tx = self.store.begin().await?;
        let forgot = tx.forget_backups(&work.forget, now).await?;
        tx.mark_unreferenced((work.read.prefix, *plan), &garbage, now).await?;
        let key = format!("sweep:{plan}");
        let (repository, workload) = work.read.prefix;
        let sweep = NewJob {
            key: &key,
            kind: SWEEP,
            payload: serde_json::json!({
                "repository": repository.to_string(),
                "workload": workload.to_string(),
                "plan": plan.to_string(),
            }),
            now,
            due: now + GRACE,
            max_attempts: SWEEP_ATTEMPTS,
        };
        tx.enqueue_job(&sweep).await?;
        let after = details([("forgotten", forgot.into()), ("marked", garbage.len().into())]);
        self.complete_control_step(
            &mut tx,
            (job, worker),
            (*plan, step),
            (Outcome::Succeeded, after),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Read access to `workload`'s backups or streams in `repo` for the control plane itself, for
    /// `ttl`, with the workload's key.
    pub(super) async fn control_plane_access(
        &self,
        (repo, area): (&Repository, Area),
        workload: Uuid,
        ttl: Duration,
    ) -> Result<JobGrant, AppError> {
        let data_key = super::keys::JobKey::Clear(self.own_key(repo, workload).await?);
        let target = CallTarget {
            organization: repo.organization,
            kind: ProviderType::BackupTarget,
            name: repo.provider.clone(),
        };
        let place = match area {
            Area::Backups => repo.workload_prefix(workload),
            Area::Streams => repo.stream_prefix(workload),
        };
        let answer = self.grant_backup_access(&target, &place, AccessMode::Read, ttl).await?;
        match answer.outcome {
            korpis_core::extension::CallOutcome::Access(access) => {
                Ok(JobGrant { repository: repo.id, workload, data_key, access, content: None })
            }
            korpis_core::extension::CallOutcome::Failed(f) => {
                Err(AppError::Unavailable { what: "the backup target", detail: f.reason })
            }
            _ => {
                Err(AppError::Unavailable { what: "the backup target", detail: "malformed".into() })
            }
        }
    }

    /// Whether `workload` has a key in the repository: whether any backup job ever began.
    async fn has_key(&self, (repository, workload): Prefix) -> Result<bool, AppError> {
        let mut tx = self.store.begin().await?;
        let key = crate::ports::BackupWrite::workload_key(&mut tx, repository, workload).await?;
        tx.commit().await?;
        Ok(key.is_some())
    }
}
