// SPDX-License-Identifier: AGPL-3.0-or-later

//! A backup step (§5 of docs/06-storage.md), which outlasts many passes of the loop: each pass
//! asks the helper where the step's job is. A job it does not know is started, with the key and
//! access the control plane grants this node for this step; a running one is waited for, without
//! holding up any other workload; a finished one is reported in the journal. The job is named
//! after the backup, so a pass after a restart of the agent finds the one already running, and it
//! is forgotten, releasing the snapshot the helper took for it, once its plan has left the set
//! (`jobs`).

use super::act::Failure;
use super::step::{After, detail, flag};
use super::{Reconciler, text};
use crate::host::Host;
use crate::jobs::NoAccess;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::v1 as pb;

/// How many objects a job moves at once.
const CONCURRENCY: u32 = 8;

fn failed(e: impl std::fmt::Display) -> Failure {
    Failure(e.to_string())
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Moves backup step `s` of plan `plan` for `a` on; its result once it ended, `None` while
    /// it has not.
    pub(super) async fn backup_step(
        &mut self,
        a: &agent::Assignment,
        plan: &str,
        s: &pb::Step,
    ) -> Result<Option<After>, Failure> {
        let backup = detail(s, "backup").ok_or_else(|| failed("a backup step names its backup"))?;
        let job = super::jobs::job_of(s).unwrap_or_default();
        let status = self.host.job_status(&job).await?;
        match status.state.as_known() {
            Some(helper::JobState::JOB_STATE_UNKNOWN) => {
                self.start_backup(a, (plan, s), &job).await
            }
            Some(helper::JobState::JOB_STATE_RUNNING) => Ok(None),
            Some(helper::JobState::JOB_STATE_FINISHED) => {
                let result = status.result.into_option().unwrap_or_default();
                self.backup_ended(&a.workload, backup, result).await.map(Some)
            }
            _ => {
                let lost = korpis_proto::korpis::backup::v1::JobResult {
                    error: "the job ended without a result".into(),
                    ..Default::default()
                };
                self.backup_ended(&a.workload, backup, lost).await.map(Some)
            }
        }
    }

    /// Starts the job, if the control plane grants what it needs; `None`, since it now runs.
    async fn start_backup(
        &self,
        a: &agent::Assignment,
        (plan, s): (&str, &pb::Step),
        job: &str,
    ) -> Result<Option<After>, Failure> {
        let grant = match self.wiring.jobs.poll(plan, s.index) {
            None => return Ok(None),
            Some(Ok(g)) => g,
            Some(Err(NoAccess::Refused(why))) => return Err(failed(format!("no access: {why}"))),
            Some(Err(NoAccess::Later(why))) => {
                tracing::info!(%job, %why, "the backup waits for access");
                return Ok(None);
            }
        };
        let work = helper::BackupVolume {
            volume: detail(s, "helper_id").unwrap_or_default().to_owned(),
            take_snapshot: flag(s, "consistent"),
            backup: detail(s, "backup").unwrap_or_default().to_owned(),
            parent: detail(s, "parent").unwrap_or_default().to_owned(),
            content: self.recipe_content(a).await,
            ..Default::default()
        };
        let request = helper::StartJobRequest {
            job: job.to_owned(),
            work: Some(Work::Backup(Box::new(work))),
            data_key: grant.data_key,
            store: grant.store,
            concurrency: CONCURRENCY,
            ..Default::default()
        };
        self.host.start_job(&request).await?;
        tracing::info!(%job, "backup started");
        Ok(None)
    }

    /// Reports how the job ended; what the step leaves, or why it failed.
    async fn backup_ended(
        &self,
        workload: &str,
        backup: &str,
        result: korpis_proto::korpis::backup::v1::JobResult,
    ) -> Result<After, Failure> {
        let (ok, error) = (result.ok, result.error.clone());
        let stats = result
            .backup
            .as_option()
            .and_then(|b| b.stats.as_option())
            .cloned()
            .unwrap_or_default();
        let e = agent::BackupEvent {
            workload: workload.to_owned(),
            backup: backup.to_owned(),
            result: result.into(),
            ..Default::default()
        };
        self.wiring
            .journal
            .append(Body::Backup(Box::new(e)))
            .await
            .map_err(|e| failed(format!("backup {backup} could not be reported: {e}")))?;
        if !ok {
            return Err(failed(format!("the backup failed: {error}")));
        }
        let mut after = After::new();
        after.insert("backup".into(), text(backup));
        after.insert("bytes".into(), text(&stats.bytes.to_string()));
        after.insert("bytes_new".into(), text(&stats.bytes_new.to_string()));
        Ok(after)
    }
}
