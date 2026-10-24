// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block device's backup read over the data path: the handler of `backup.browse` and
//! `backup.read` (§5.4 of docs/06-storage.md). Reading inside such a backup is parsing a
//! filesystem the tenant's guest wrote, which only a guest's kernel does, so the node stages the
//! image and the workload's driver serves it read-only from a service guest of the image's own
//! (§4.4 of docs/04-runtimes.md).
//!
//! The node holds neither the repository's credentials nor what says which workload a backup is
//! of, so it asks the control plane, as a restore step does, naming the session's backup and
//! passing its token: the control plane verifies the token again, checks that the backup is the
//! workload's and holds an image, records the read, grants read access to the workload's prefix,
//! and names the volume the image is staged beside. The helper restores the image into a reading
//! place of the job's own beside that volume, the driver is opened on it, and the session's frames
//! are carried as a file session's are, a `backup.browse` session ended at its first read of a
//! file's bytes. The session is journaled at both ends, as a file session is.
//!
//! Sessions on one backup share one copy, its job named by the backup; when the last of them ends
//! the job is forgotten, which discards the copy. The reconciler forgets every ended job no plan
//! names, and leaves alone those a live session holds ([`Reading`]); a session that ends while its
//! image is still being restored leaves the restore to finish, and the reconciler's next sweep
//! discards it, as it does every reading job after a restart.

use super::files::tally::Tally;
use super::files::{carried, ended, record, text};
use super::handler::{Ended, Handler, Io, Opened, Serving};
use crate::held::HeldSet;
use crate::host::Host;
use crate::jobs::{JobAccesses, NoAccess};
use crate::journal::JournalHandle;
use crate::runtime::Runtime;
use buffa::Message as _;
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::Principal;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// The action that lists a backup's files.
pub const BROWSE: &str = "backup.browse";
/// The action that reads them, and lists them.
pub const READ: &str = "backup.read";

/// What a reading job's identifier begins with.
const JOB: &str = "read-";
/// How long a session waits for the control plane's answer.
const ACCESS_WITHIN: Duration = Duration::from_secs(120);
/// How often it looks for the answer, and at the restore.
const LOOK_EVERY: Duration = Duration::from_millis(250);
/// The restore job's parallel transfers, as a restore step's.
const CONCURRENCY: u32 = 8;

/// The reading jobs live sessions hold, each with how many sessions hold it: sessions on one
/// backup share its copy, so however many are open it is staged once. The reconciler forgets no
/// job one holds.
#[derive(Debug, Clone, Default)]
pub struct Reading(Arc<tokio::sync::Mutex<BTreeMap<String, usize>>>);

impl Reading {
    /// Whether a live session holds job `job`; held, too, while one is taking or letting it go.
    #[must_use]
    pub fn holds(&self, job: &str) -> bool {
        self.0.try_lock().map_or(true, |held| held.contains_key(job))
    }

    /// Holds `job` for a session, until the hold is released or dropped.
    async fn hold(&self, job: &str) -> Hold {
        *self.0.lock().await.entry(job.to_owned()).or_default() += 1;
        Hold { reading: Some(self.clone()), job: job.to_owned() }
    }
}

/// Gives one session's hold on `job` back in `held`; whether it was the last.
fn given_back(held: &mut BTreeMap<String, usize>, job: &str) -> bool {
    let left = held.get_mut(job).map_or(0, |n| {
        *n = n.saturating_sub(1);
        *n
    });
    if left == 0 {
        held.remove(job);
    }
    left == 0
}

/// A session's hold on its job.
struct Hold {
    reading: Option<Reading>,
    job: String,
}

impl Hold {
    /// Lets the job go, and forgets it through `host` when no other session holds it: under the
    /// lock, so a session taking it meanwhile waits, and then stages it afresh.
    async fn release(mut self, host: &impl Host) {
        let Some(reading) = self.reading.take() else { return };
        let mut held = reading.0.lock().await;
        if given_back(&mut held, &self.job)
            && let Err(e) = host.forget_job(&self.job).await
        {
            tracing::info!(job = %self.job, error = %e, "a reading job is left to the reconciler");
        }
    }
}

impl Drop for Hold {
    /// A session dropped before it let its job go gives its hold back from a task of its own;
    /// the reconciler forgets the job once no session holds it.
    fn drop(&mut self) {
        let Some(reading) = self.reading.take() else { return };
        let job = std::mem::take(&mut self.job);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                given_back(&mut *reading.0.lock().await, &job);
            });
        }
    }
}

/// Serves sessions reading block devices' backups.
#[derive(Debug, Clone)]
pub struct BackupSessions<H, R> {
    host: H,
    runtime: R,
    held: watch::Receiver<HeldSet>,
    journal: JournalHandle,
    access: Arc<JobAccesses>,
    reading: Reading,
}

impl<H: Host + Clone + Send + Sync + 'static, R: Runtime + Clone> BackupSessions<H, R> {
    /// Stages images through `host` and reads them through `runtime`, for the workloads `held`
    /// names, asking `access` for each job's access and holding each job in `reading`.
    #[must_use]
    pub fn new(
        (host, runtime): (H, R),
        held: watch::Receiver<HeldSet>,
        journal: JournalHandle,
        (access, reading): (Arc<JobAccesses>, Reading),
    ) -> Self {
        Self { host, runtime, held, journal, access, reading }
    }
}

impl<H: Host + Clone + Send + Sync + 'static, R: Runtime + Clone> Handler for BackupSessions<H, R> {
    fn serve(&self, opened: Opened, io: Io) -> Serving {
        Box::pin(serve(self.clone(), opened, io))
    }
}

/// A browse session's requests: anything but a file's bytes.
fn browsing(r: &pb::FileRequest) -> Result<(), Ended> {
    use korpis_proto::korpis::v1::__buffa::oneof::file_request::Request;
    match r.request {
        Some(Request::Read(_)) => Err(ended("refused", "browsing a backup reads no file's bytes")),
        _ => Ok(()),
    }
}

async fn serve<H: Host, R: Runtime>(
    b: BackupSessions<H, R>,
    opened: Opened,
    io: Io,
) -> Result<(), Ended> {
    let actor = Principal::from_key(&opened.subject)
        .map_err(|e| ended("refused", format!("the session's subject: {e}")))?;
    let asked = pb::BackupFiles::decode_from_slice(&opened.params)
        .map_err(|e| ended("invalid", format!("the params are not a BackupFiles: {e}")))?;
    // The helper names a backup by its identifier's uuid, as a restore step does.
    let named = Id::parse(IdKind::Backup, &asked.backup)
        .map_err(|e| ended("invalid", format!("the params' backup: {e}")))?
        .uuid()
        .to_string();
    let (driver, ids) = workload(&b.held.borrow(), &opened.workload)?;
    let mut fields = BTreeMap::new();
    fields.insert("backup".to_owned(), text(&asked.backup));
    record(&b.journal, (&opened, &actor), ("opened", fields.clone(), None)).await?;
    // One copy of a backup on the node, whichever sessions read it.
    let job = format!("{JOB}{}", named.replace('-', ""));
    let hold = b.reading.hold(&job).await;
    let mut tally = Tally::default();
    let carried = async {
        let grant = access(&b, &opened, &asked.backup).await?;
        let filesystem = grant.image_filesystem.clone();
        let image = staged(&b.host, &job, &named, grant).await?;
        let staged = driver::StagedImage {
            image,
            filesystem,
            workload: opened.workload.clone(),
            uid_base: ids.0,
            uid_count: ids.1,
            ..Default::default()
        };
        let unavailable = |e: crate::runtime::RuntimeError| ended("unavailable", e.to_string());
        let worker = b.runtime.image_files(&driver, &staged).await.map_err(unavailable)?;
        let allowed = if opened.action == BROWSE { browsing } else { |_: &_| Ok(()) };
        carried(worker, io, &mut tally, allowed).await
    }
    .await;
    hold.release(&b.host).await;
    fields.extend(tally.fields());
    record(&b.journal, (&opened, &actor), ("ended", fields, carried.as_ref().err())).await?;
    carried
}

/// The driver of `workload`, held here, and its ids.
fn workload(held: &HeldSet, workload: &str) -> Result<(String, (u32, u32)), Ended> {
    let a = held
        .assignments
        .get(workload)
        .ok_or_else(|| ended("unavailable", "the workload is not held here"))?;
    Ok((crate::reconcile::driver_of(a).to_owned(), (a.uid_base, a.uid_count)))
}

/// What the control plane grants the session reading `backup`, waited for.
async fn access<H, R>(
    b: &BackupSessions<H, R>,
    opened: &Opened,
    backup: &str,
) -> Result<agent::JobGrant, Ended> {
    let read = agent::BackupRead {
        backup: backup.to_owned(),
        token: opened.token.clone(),
        action: opened.action.clone(),
        workload: opened.workload.clone(),
        ..Default::default()
    };
    let until = Instant::now() + ACCESS_WITHIN;
    loop {
        match b.access.poll_backup_read(&opened.session, read.clone()) {
            Some(Ok(grant)) => return Ok(grant),
            Some(Err(NoAccess::Refused(why))) => return Err(ended("refused", why)),
            Some(Err(NoAccess::Later(why))) => return Err(ended("unavailable", why)),
            None if Instant::now() >= until => {
                return Err(ended("unavailable", "the control plane did not answer"));
            }
            None => tokio::time::sleep(LOOK_EVERY).await,
        }
    }
}

/// Restores `backup`'s image for reading as job `job` with `grant`, waited for; the image.
async fn staged(
    host: &impl Host,
    job: &str,
    backup: &str,
    grant: agent::JobGrant,
) -> Result<String, Ended> {
    let failed = |e: crate::host::HostError| ended("unavailable", e.to_string());
    let work = helper::RestoreVolume {
        volume: grant.volume,
        backup: backup.to_owned(),
        reading: true,
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
    host.start_job(&request).await.map_err(failed)?;
    loop {
        let status = host.job_status(job).await.map_err(failed)?;
        match status.state.as_known() {
            Some(helper::JobState::JOB_STATE_RUNNING) => tokio::time::sleep(LOOK_EVERY).await,
            Some(helper::JobState::JOB_STATE_FINISHED) => {
                let result = status.result.into_option().unwrap_or_default();
                let verified = result.restored.as_option().is_some_and(|r| r.verified);
                if !(result.ok && verified) {
                    let why =
                        format!("the backup's image was not restored whole: {}", result.error);
                    return Err(ended("unavailable", why));
                }
                return Ok(status.image);
            }
            _ => return Err(ended("unavailable", "the backup's image restore was lost")),
        }
    }
}
