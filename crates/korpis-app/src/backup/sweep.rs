// SPDX-License-Identifier: AGPL-3.0-or-later

//! The second phase of a prune (§5.5 of docs/06-storage.md): what a prune marked, deleted with
//! the target's own credential once its grace has passed. The sweep holds the workload's prefix,
//! so no backup into it is proposed meanwhile, and begins only when no backup into it is open and
//! no write access to it that the control plane granted can still be used: a job whose plan ended
//! while it ran is a writer too. So every backup that could have taken a marked object as
//! already there has finished and has a record, and what is deleted is what was marked and is
//! still referenced by no backup kept now, the references read again; a name referenced again
//! is unmarked and kept.

use super::prune::{GRACE, PruneRead};
use crate::App;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::ports::{
    Clock, EffectWrite as _, Job, JobEnd, JobWrite as _, Prefix, PruneWrite as _, RecipeSource,
    Store, Tx as _,
};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::extension::{CallOutcome, MAX_KEYS, ProviderType};
use korpis_core::principal::{Principal, SystemActor};
use std::collections::BTreeSet;
use std::time::Duration;
use uuid::Uuid;

/// How long a sweep that found the prefix busy waits before looking again.
const BUSY: Duration = Duration::from_secs(300);

/// How long the record of access is kept after it ended.
const ACCESS_KEPT: Duration = Duration::from_secs(24 * 3600);

/// What a sweep deletes from.
#[derive(Debug)]
pub struct SweepWork {
    /// The prune plan whose marks it sweeps.
    pub plan: Uuid,
    /// The names that plan marked, their grace passed.
    pub marked: Vec<String>,
    /// What it reads.
    pub read: PruneRead,
}

/// What a sweep job is to do now.
#[derive(Debug)]
pub enum Sweep {
    /// Delete, once the references are read.
    Go(Box<SweepWork>),
    /// Nothing more: it was put back for later, or had nothing to do.
    Done,
}

fn sweep_of(job: &Job) -> Option<(Prefix, Uuid)> {
    let id = |k: &str| job.payload.get(k)?.as_str()?.parse::<Uuid>().ok();
    Some(((id("repository")?, id("workload")?), id("plan")?))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Begins sweep job `job` of `worker`: it holds the prefix and says what to read, or it is
    /// put back for when the prefix may be free.
    pub async fn begin_sweep(&self, (job, worker): (&Job, &str)) -> Result<Sweep, AppError> {
        let Some((prefix, plan)) = sweep_of(job) else {
            self.end_job_idle(job, worker).await?;
            return Ok(Sweep::Done);
        };
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        let writable = self.store.writable_until(prefix, now).await?;
        let wait = match writable {
            Some(until) => Some(until),
            None if !tx.hold_for_sweep(prefix, job.id).await? => Some(now + BUSY),
            None => None,
        };
        if let Some(at) = wait {
            tx.end_job((job.id, worker), &JobEnd::Defer { at }, now).await?;
            tx.commit().await?;
            return Ok(Sweep::Done);
        }
        tx.commit().await?;
        let marked = self.store.marked(prefix, plan, now - GRACE).await?;
        let repo = self.store.repository(prefix.0).await?;
        let repo = repo.ok_or(AppError::NotFound { kind: "repository" })?;
        let read = self.prune_read(&repo, prefix.1, &[]).await?;
        Ok(Sweep::Go(Box::new(SweepWork { plan, marked, read })))
    }

    /// Ends sweep job `job` of `worker`: deletes what `work` marked that is still there and that
    /// `referenced` does not name, lets the prefix go, and records it.
    pub async fn finish_sweep(
        &self,
        (job, worker): (&Job, &str),
        work: &SweepWork,
        referenced: &BTreeSet<String>,
    ) -> Result<(), AppError> {
        let there: BTreeSet<&str> = work.read.objects.iter().map(|o| o.key.as_str()).collect();
        let doomed: Vec<String> = work
            .marked
            .iter()
            .filter(|n| there.contains(n.as_str()) && !referenced.contains(*n))
            .map(|n| format!("{}{n}", work.read.place))
            .collect();
        let deleted = self.delete_all(&work.read, &doomed).await?;
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        tx.unmark(work.read.prefix, &work.marked).await?;
        tx.release_sweep(work.read.prefix, job.id).await?;
        tx.drop_ended_access(now - ACCESS_KEPT).await?;
        if !tx.end_job((job.id, worker), &JobEnd::Done, now).await? {
            return Err(AppError::conflict("job.lost", "another worker holds the job now"));
        }
        let kept = work.marked.len().saturating_sub(doomed.len());
        let after = details([
            ("plan", work.plan.to_string().into()),
            ("repository", work.read.prefix.0.to_string().into()),
            ("deleted", deleted.into()),
            ("kept", kept.into()),
        ]);
        let mut e = Effect::new(
            Principal::System(SystemActor::Reconciler),
            "backup.sweep",
            Outcome::Succeeded,
        )
        .after(after);
        e.workload = Some(work.read.prefix.1);
        tx.append_effect(&e).await?;
        tx.commit().await?;
        Ok(())
    }

    /// After sweep job `job` of `worker` failed with `error`: the prefix let go, and the sweep
    /// tried again later, or, its attempts spent, failed, its marks kept for the next prune.
    pub async fn sweep_failed(
        &self,
        (job, worker): (&Job, &str),
        error: &AppError,
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        let mut tx = self.store.begin().await?;
        if let Some((prefix, _)) = sweep_of(job) {
            tx.release_sweep(prefix, job.id).await?;
        }
        let end = if job.attempt < job.max_attempts {
            JobEnd::Retry {
                at: now + crate::control::backoff(job.attempt),
                error: error.to_string(),
            }
        } else {
            JobEnd::Failed(error.to_string())
        };
        tx.end_job((job.id, worker), &end, now).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Deletes `keys` from `read`'s target, a thousand at a time; how many it deleted.
    async fn delete_all(&self, read: &PruneRead, keys: &[String]) -> Result<u64, AppError> {
        let repo = self.store.repository(read.prefix.0).await?;
        let repo = repo.ok_or(AppError::NotFound { kind: "repository" })?;
        let target = CallTarget {
            organization: repo.organization,
            kind: ProviderType::BackupTarget,
            name: repo.provider.clone(),
        };
        let mut deleted = 0;
        for page in keys.chunks(MAX_KEYS) {
            let called = self.delete_backup_objects(&target, &read.place, page.to_vec()).await?;
            match called.outcome {
                CallOutcome::Deleted { count } => deleted += u64::from(count),
                CallOutcome::Failed(f) => {
                    return Err(AppError::Unavailable {
                        what: "the backup target",
                        detail: f.reason,
                    });
                }
                _ => {
                    let detail = "malformed".into();
                    return Err(AppError::Unavailable { what: "the backup target", detail });
                }
            }
        }
        Ok(deleted)
    }
}
