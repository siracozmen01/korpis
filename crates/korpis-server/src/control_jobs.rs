// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's own work (§6 of docs/03-state.md, §4.9 of docs/02-architecture.md): the
//! plan steps it executes, queued when their turn comes, and the sweeps prunes leave behind,
//! claimed by one replica at a time. Whatever references a backup repository holds is read here,
//! with the engine, through access the application layer granted; what to do with the answer is
//! the application's.

use crate::api::Api;
use crate::repos::repo_of;
use korpis_app::AppError;
use korpis_app::backup::prune::PruneRead;
use korpis_app::backup::prune::SWEEP;
use korpis_app::backup::sweep::Sweep;
use korpis_app::control::PLAN_STEP;
use korpis_app::ports::{Changes as _, Job, JobRead as _, Store as _, Wakeups as _};
use korpis_core::workload::plan::StepAction;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// How long a claim lasts: longer than reading a large repository's trees takes.
const LEASE: Duration = Duration::from_secs(30 * 60);

/// The least time between two looks for due steps, however often plans change.
const COALESCE: Duration = Duration::from_secs(1);

/// Queues each control-plane step when its turn comes: whenever a plan or workload changes, and
/// on the feed's periodic resync. Ends with the process.
pub(crate) async fn queue_steps(api: Arc<Api>) {
    let mut changes = api.app().store().subscribe();
    loop {
        match api.app().queue_control_steps().await {
            Ok(0) => {}
            Ok(n) => tracing::debug!(queued = n, "control-plane steps queued"),
            Err(e) => tracing::warn!(error = %e, "queueing control-plane steps failed"),
        }
        tokio::time::sleep(COALESCE).await;
        if changes.next().await.is_none() {
            return;
        }
        while let Ok(Some(_)) = tokio::time::timeout(Duration::ZERO, changes.next()).await {}
    }
}

/// Claims and does jobs, one at a time, as long as the process runs.
pub(crate) async fn work(api: Arc<Api>) {
    let worker = format!("control-plane-{}", uuid::Uuid::now_v7());
    let mut wake = api.app().store().jobs();
    loop {
        let now = api.app().now();
        match api.app().store().claim_job(&worker, &[PLAN_STEP, SWEEP], (now, LEASE)).await {
            Ok(Some(job)) => {
                run(&api, &worker, &job).await;
                continue;
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "claiming a job failed"),
        }
        if wake.next().await.is_none() {
            return;
        }
    }
}

async fn run(api: &Api, worker: &str, job: &Job) {
    let done = match job.kind.as_str() {
        PLAN_STEP => plan_step(api, worker, job).await,
        SWEEP => sweep(api, worker, job).await,
        _ => Ok(()),
    };
    if let Err(e) = done {
        tracing::warn!(error = %e, job = %job.id, kind = %job.kind, "a job failed");
    }
}

/// What the backups `read` keeps reference, read through its access.
async fn referenced(read: &PruneRead) -> Result<BTreeSet<String>, AppError> {
    let Some(grant) = &read.grant else { return Ok(BTreeSet::new()) };
    let unavailable =
        |detail: String| AppError::Unavailable { what: "the backup repository", detail };
    let repo = repo_of(grant).map_err(|e| unavailable(e.to_string()))?;
    korpis_backup::live::referenced(&repo, &read.kept())
        .await
        .map_err(|e| unavailable(e.to_string()))
}

async fn plan_step(api: &Api, worker: &str, job: &Job) -> Result<(), AppError> {
    let app = api.app();
    let Some((plan, step)) = app.step_to_do(job).await? else {
        return app.end_job_idle(job, worker).await;
    };
    if step.action != StepAction::PruneBackups {
        let why = AppError::Internal(format!(
            "the control plane does not execute {}",
            step.action.as_str()
        ));
        return app.control_step_failed((job, worker), &why, false).await;
    }
    let attempt = async {
        let work = app.begin_prune((plan, step)).await?;
        let refs = referenced(&work.read).await?;
        app.finish_prune((job, worker), &work, &refs).await
    };
    match attempt.await {
        Ok(()) => Ok(()),
        Err(e) => app.control_step_failed((job, worker), &e, true).await,
    }
}

async fn sweep(api: &Api, worker: &str, job: &Job) -> Result<(), AppError> {
    let app = api.app();
    let attempt = async {
        let Sweep::Go(work) = app.begin_sweep((job, worker)).await? else { return Ok(()) };
        let refs = referenced(&work.read).await?;
        app.finish_sweep((job, worker), &work, &refs).await
    };
    match attempt.await {
        Ok(()) => Ok(()),
        Err(e) => app.sweep_failed((job, worker), &e).await,
    }
}
