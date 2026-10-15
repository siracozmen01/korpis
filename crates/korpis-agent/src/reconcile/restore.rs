// SPDX-License-Identifier: AGPL-3.0-or-later

//! A restore's steps (§5.4 of docs/06-storage.md). Restoring outlasts many passes of the loop,
//! as a backup does: each pass asks the helper where the step's job is, starts it with the key
//! and read access the control plane grants this node for this step when the helper knows none,
//! and waits while it runs. A restore that ended whole and verified completes the step; one that
//! did not fails it. Replacing a volume is a later step of the same plan, once the workload is
//! stopped: the helper makes the volume what the job staged, which it does once however often it
//! is asked.

use super::act::{Failure, volume_id};
use super::jobs::job_of;
use super::step::{After, detail, flag};
use super::{Reconciler, text};
use crate::host::Host;
use crate::jobs::NoAccess;
use crate::runtime::Runtime;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::backup::v1 as backup;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::v1 as pb;

/// How many objects a job moves at once.
const CONCURRENCY: u32 = 8;

fn failed(e: impl std::fmt::Display) -> Failure {
    Failure(e.to_string())
}

fn job(s: &pb::Step) -> Result<String, Failure> {
    job_of(s).ok_or_else(|| failed("a restore step names its job"))
}

/// What the step leaves of a restore that ended, or why it failed.
fn ended(result: &backup::JobResult) -> Result<After, Failure> {
    let restored = result.restored.as_option();
    if !result.ok {
        return Err(failed(format!("the restore failed: {}", result.error)));
    }
    let Some(r) = restored.filter(|r| r.verified) else {
        return Err(failed("the restore was not verified"));
    };
    let mut after = After::new();
    after.insert("files".into(), text(&r.files.to_string()));
    after.insert("bytes".into(), text(&r.bytes.to_string()));
    after.insert("verified".into(), text("true"));
    Ok(after)
}

impl<R: Runtime, H: Host> Reconciler<R, H> {
    /// Moves restore step `s` of plan `plan` on; its result once the job ended, `None` while it
    /// has not.
    pub(super) async fn restore_step(
        &mut self,
        a: &agent::Assignment,
        plan: &str,
        s: &pb::Step,
    ) -> Result<Option<After>, Failure> {
        let job = job(s)?;
        let status = self.host.job_status(&job).await?;
        match status.state.as_known() {
            Some(helper::JobState::JOB_STATE_UNKNOWN) => {
                self.start_restore((a, plan), s, &job).await
            }
            Some(helper::JobState::JOB_STATE_RUNNING) => Ok(None),
            Some(helper::JobState::JOB_STATE_FINISHED) => {
                ended(&status.result.into_option().unwrap_or_default()).map(Some)
            }
            _ => Err(failed("the restore ended without a result")),
        }
    }

    /// Starts the job, if the control plane grants what it needs; `None`, since it now runs.
    async fn start_restore(
        &self,
        (a, plan): (&agent::Assignment, &str),
        s: &pb::Step,
        job: &str,
    ) -> Result<Option<After>, Failure> {
        let grant = match self.wiring.jobs.poll(plan, s.index) {
            None => return Ok(None),
            Some(Ok(g)) => g,
            Some(Err(NoAccess::Refused(why))) => return Err(failed(format!("no access: {why}"))),
            Some(Err(NoAccess::Later(why))) => {
                tracing::info!(%job, %why, "the restore waits for access");
                return Ok(None);
            }
        };
        // A volume the plan just created is known by its name, as the node made it; any other,
        // by the helper's identifier its node reported.
        let volume = match detail(s, "helper_id") {
            Some(id) => id.to_owned(),
            None => volume_id(a, detail(s, "volume").unwrap_or_default()),
        };
        let work = helper::RestoreVolume {
            volume,
            backup: detail(s, "backup").unwrap_or_default().to_owned(),
            path: detail(s, "path").unwrap_or_default().to_owned(),
            into: detail(s, "into").unwrap_or_default().to_owned(),
            replace: flag(s, "replace"),
            ..Default::default()
        };
        let request = helper::StartJobRequest {
            job: job.to_owned(),
            work: Some(Work::Restore(Box::new(work))),
            data_key: grant.data_key,
            store: grant.store,
            content_store: grant.content_store,
            concurrency: CONCURRENCY,
            ..Default::default()
        };
        self.host.start_job(&request).await?;
        tracing::info!(%job, "restore started");
        Ok(None)
    }

    /// Makes the volume what the plan's restore staged.
    pub(super) async fn replace_step(&self, s: &pb::Step) -> Result<After, Failure> {
        let job = job(s)?;
        self.host.commit_job(&job).await?;
        let mut after = After::new();
        after.insert("replaced".into(), text("true"));
        Ok(after)
    }
}
