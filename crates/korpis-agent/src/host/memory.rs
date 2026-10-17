// SPDX-License-Identifier: AGPL-3.0-or-later

//! A [`Host`] that keeps its records in memory, for tests of the agent and of what drives it.

use super::{Host, HostError, SnapshotOp};
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::collections::{BTreeMap, BTreeSet};
use std::future::{Future, ready};
use std::sync::{Arc, Mutex};

mod files;

#[derive(Debug, Default)]
struct Kept {
    volumes: BTreeMap<String, String>,
    content: BTreeSet<String>,
    /// Each artifact's files, as a test made them available.
    files: BTreeMap<String, BTreeMap<String, String>>,
    /// Content held, by digest, as a test offered it.
    blobs: BTreeMap<String, Vec<u8>>,
    ids: BTreeMap<String, (u32, u32)>,
    snapshots: BTreeSet<(String, String)>,
    jobs: BTreeMap<String, helper::JobStatusResponse>,
    /// What each job was started with.
    started: BTreeMap<String, helper::StartJobRequest>,
    /// The networks asked for, by workload; none is made, so none has a namespace.
    networks: BTreeMap<String, helper::EnsureNetworkRequest>,
    /// The socket the ingress proxy was last started to connect to.
    ingress: Option<std::path::PathBuf>,
    /// How many times it was started.
    ingress_starts: u32,
    /// Where each volume a file session names is served from, as a directory of its own.
    file_root: Option<std::path::PathBuf>,
    /// The file sessions opened.
    opened_files: Vec<helper::OpenFilesRequest>,
}

/// A helper and content store that keep their records in memory; clones share them.
#[derive(Debug, Clone, Default)]
pub struct InMemoryHost {
    kept: Arc<Mutex<Kept>>,
}

impl InMemoryHost {
    /// Nothing kept.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn with<T>(&self, f: impl FnOnce(&mut Kept) -> T) -> Result<T, HostError> {
        let mut kept = self.kept.lock().map_err(|_| HostError("poisoned".into()))?;
        Ok(f(&mut kept))
    }

    /// The volumes, by identifier, with their paths.
    #[must_use]
    pub fn volumes(&self) -> BTreeMap<String, String> {
        self.with(|k| k.volumes.clone()).unwrap_or_default()
    }

    /// The content fetched.
    #[must_use]
    pub fn content(&self) -> BTreeSet<String> {
        self.with(|k| k.content.clone()).unwrap_or_default()
    }

    /// Makes the artifact `reference` available with `files`, by path and digest.
    pub fn offer_files(&self, reference: &str, files: BTreeMap<String, String>) {
        let _ = self.with(|k| k.files.insert(reference.to_owned(), files));
    }

    /// What job `job` was started with, if it was.
    #[must_use]
    pub fn started(&self, job: &str) -> Option<helper::StartJobRequest> {
        self.with(|k| k.started.get(job).cloned()).ok().flatten()
    }

    /// The socket the ingress proxy was last started to connect to, while it runs, and how many
    /// times it was started.
    #[must_use]
    pub fn ingress(&self) -> (Option<std::path::PathBuf>, u32) {
        self.with(|k| (k.ingress.clone(), k.ingress_starts)).unwrap_or_default()
    }

    /// Every job started, by name.
    #[must_use]
    pub fn started_jobs(&self) -> Vec<String> {
        self.with(|k| k.started.keys().cloned().collect()).unwrap_or_default()
    }

    /// Holds `bytes` as content `digest`.
    pub fn offer_content(&self, digest: &str, bytes: &[u8]) {
        let _ = self.with(|k| k.blobs.insert(digest.to_owned(), bytes.to_vec()));
    }

    /// The snapshots, by volume and snapshot.
    #[must_use]
    pub fn snapshots(&self) -> BTreeSet<(String, String)> {
        self.with(|k| k.snapshots.clone()).unwrap_or_default()
    }

    /// The id ranges recorded, by workload: base and count.
    #[must_use]
    pub fn ids(&self) -> BTreeMap<String, (u32, u32)> {
        self.with(|k| k.ids.clone()).unwrap_or_default()
    }
}

impl Host for InMemoryHost {
    fn ensure_volume(
        &self,
        request: &helper::EnsureVolumeRequest,
    ) -> impl Future<Output = Result<helper::EnsureVolumeResponse, HostError>> + Send {
        let volume = request.volume.clone();
        ready(self.with(|k| {
            let created = !k.volumes.contains_key(&volume);
            let path = k.volumes.entry(volume.clone()).or_insert(format!("/memory/{volume}"));
            helper::EnsureVolumeResponse {
                host_path: path.clone(),
                created,
                storage_class: request.storage_class.clone(),
                ..Default::default()
            }
        }))
    }

    fn delete_volume(&self, volume: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.volumes.remove(volume);
        }))
    }

    fn snapshot(
        &self,
        op: SnapshotOp,
        volume: &str,
        snapshot: &str,
    ) -> impl Future<Output = Result<(), HostError>> + Send {
        let key = (volume.to_owned(), snapshot.to_owned());
        ready(
            self.with(|k| match op {
                SnapshotOp::Take if k.volumes.contains_key(volume) => {
                    k.snapshots.insert(key);
                    Ok(())
                }
                SnapshotOp::Rollback if k.snapshots.contains(&key) => Ok(()),
                SnapshotOp::Delete => {
                    k.snapshots.remove(&key);
                    Ok(())
                }
                _ => Err(HostError(format!("no volume {volume} with snapshot {snapshot}"))),
            })
            .and_then(|r| r),
        )
    }

    fn fetch(&self, reference: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.content.insert(reference.to_owned());
        }))
    }

    fn fetch_files(
        &self,
        reference: &str,
    ) -> impl Future<Output = Result<BTreeMap<String, String>, HostError>> + Send {
        let found = self.with(|k| {
            k.content.insert(reference.to_owned());
            k.files.get(reference).cloned().unwrap_or_default()
        });
        ready(found)
    }

    fn content_size(
        &self,
        digest: &str,
    ) -> impl Future<Output = Result<Option<u64>, HostError>> + Send {
        let held = self.with(|k| k.blobs.get(digest).map(Vec::len)).ok().flatten();
        ready(Ok(held.map(|n| u64::try_from(n).unwrap_or(u64::MAX))))
    }

    fn read_content(
        &self,
        digest: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<u8>, HostError>> + Send {
        let held = self.with(|k| k.blobs.get(digest).cloned()).ok().flatten();
        ready(match held {
            Some(b) if b.len() <= limit => Ok(b),
            Some(_) => Err(HostError(format!("{digest} is over {limit} bytes"))),
            None => Err(HostError(format!("{digest} is not held"))),
        })
    }

    fn fetch_url(
        &self,
        _url: &str,
        digest: &str,
    ) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.content.insert(digest.to_owned());
        }))
    }

    fn ensure_ids(
        &self,
        workload: &str,
        base: u32,
        count: u32,
    ) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.ids.insert(workload.to_owned(), (base, count));
        }))
    }

    fn storage(&self) -> impl Future<Output = Result<Vec<pb::StorageClassInfo>, HostError>> + Send {
        ready(Ok(Vec::new()))
    }

    /// A job finishes as it starts: a backup with a record, a restore verified.
    fn start_job(
        &self,
        request: &helper::StartJobRequest,
    ) -> impl Future<Output = Result<bool, HostError>> + Send {
        let finished = finished_at_once(request);
        ready(self.with(|k| {
            let started = !k.jobs.contains_key(&request.job);
            k.jobs.entry(request.job.clone()).or_insert(finished);
            k.started.insert(request.job.clone(), request.clone());
            started
        }))
    }

    fn job_status(
        &self,
        job: &str,
    ) -> impl Future<Output = Result<helper::JobStatusResponse, HostError>> + Send {
        ready(self.with(|k| {
            k.jobs.get(job).cloned().unwrap_or_else(|| helper::JobStatusResponse {
                state: helper::JobState::JOB_STATE_UNKNOWN.into(),
                ..Default::default()
            })
        }))
    }

    fn forget_job(&self, job: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        ready(self.with(|k| {
            k.jobs.remove(job);
        }))
    }

    fn commit_job(&self, job: &str) -> impl Future<Output = Result<(), HostError>> + Send {
        let committed = self.with(|k| {
            let restored = k.jobs.get(job).and_then(|s| s.result.as_option());
            match restored.and_then(|r| r.restored.as_option()) {
                Some(r) if r.verified => Ok(()),
                _ => Err(HostError(format!("job {job} is no whole restore"))),
            }
        });
        ready(committed.and_then(|r| r))
    }

    fn list_jobs(&self) -> impl Future<Output = Result<Vec<helper::JobSummary>, HostError>> + Send {
        ready(self.with(|k| {
            let summary = |(job, s): (&String, &helper::JobStatusResponse)| helper::JobSummary {
                job: job.clone(),
                state: s.state,
                ..Default::default()
            };
            k.jobs.iter().map(summary).collect()
        }))
    }

    fn ensure_network(
        &self,
        request: &helper::EnsureNetworkRequest,
    ) -> impl Future<Output = Result<helper::EnsureNetworkResponse, HostError>> + Send {
        ready(self.with(|k| {
            k.networks.insert(request.workload.clone(), request.clone());
            helper::EnsureNetworkResponse::default()
        }))
    }

    fn release_network(
        &self,
        workload: &str,
    ) -> impl Future<Output = Result<bool, HostError>> + Send {
        ready(self.with(|k| k.networks.remove(workload).is_some()))
    }

    fn open_resolver(
        &self,
        _workload: &str,
    ) -> impl Future<Output = Result<Vec<std::os::fd::OwnedFd>, HostError>> + Send {
        ready(Err(HostError("the in-memory host makes no networks".into())))
    }

    /// Listeners on loopback in place of each address's ports 80 and 443, so a test reaches
    /// what the agent serves on them.
    fn open_ingress(
        &self,
        addresses: &[String],
    ) -> impl Future<Output = Result<Vec<std::os::fd::OwnedFd>, HostError>> + Send {
        let opened = (0..addresses.len() * 2)
            .map(|_| {
                std::net::TcpListener::bind("127.0.0.1:0")
                    .map(std::os::fd::OwnedFd::from)
                    .map_err(|e| HostError(e.to_string()))
            })
            .collect();
        ready(opened)
    }

    /// Records `control`, where a test then plays the proxy.
    fn start_ingress(
        &self,
        control: &std::path::Path,
    ) -> impl Future<Output = Result<helper::StartIngressResponse, HostError>> + Send {
        let recorded = self.with(|k| {
            k.ingress = Some(control.to_path_buf());
            k.ingress_starts += 1;
        });
        ready(recorded.map(|()| helper::StartIngressResponse::default()))
    }

    fn stop_ingress(&self) -> impl Future<Output = Result<bool, HostError>> + Send {
        ready(self.with(|k| k.ingress.take().is_some()))
    }

    fn open_files(
        &self,
        request: &helper::OpenFilesRequest,
    ) -> impl Future<Output = Result<std::os::fd::OwnedFd, HostError>> + Send {
        ready(self.serve_files(request))
    }

    fn describe_network(
        &self,
    ) -> impl Future<Output = Result<helper::DescribeNetworkResponse, HostError>> + Send {
        ready(Ok(helper::DescribeNetworkResponse {
            why_not: "the in-memory host makes no networks".into(),
            ..Default::default()
        }))
    }
}

/// The status of `request`'s job once it finished as the in-memory host finishes jobs.
fn finished_at_once(request: &helper::StartJobRequest) -> helper::JobStatusResponse {
    use korpis_proto::korpis::backup::v1 as backup;
    use korpis_proto::korpis::helper::v1::__buffa::oneof::start_job_request::Work;
    let mut result = backup::JobResult { ok: true, ..Default::default() };
    match &request.work {
        Some(Work::Backup(b)) => {
            let stats = backup::BackupStats { files: 1, ..Default::default() };
            result.backup = backup::Backup {
                id: b.backup.clone(),
                consistent: b.take_snapshot || !b.snapshot.is_empty(),
                stats: stats.into(),
                ..Default::default()
            }
            .into();
        }
        Some(Work::Restore(_)) => {
            result.restored = backup::Restored { verified: true, ..Default::default() }.into();
        }
        Some(Work::Install(w)) => {
            // Every action runs, and nothing is written: the in-memory host has no files.
            let done = u32::try_from(w.actions.len()).unwrap_or(u32::MAX);
            let install = korpis_proto::korpis::install::v1::InstallResult {
                ok: true,
                done,
                ..Default::default()
            };
            return helper::JobStatusResponse {
                state: helper::JobState::JOB_STATE_FINISHED.into(),
                install: install.into(),
                progress: done,
                ..Default::default()
            };
        }
        None => result.ok = false,
    }
    helper::JobStatusResponse {
        state: helper::JobState::JOB_STATE_FINISHED.into(),
        result: result.into(),
        ..Default::default()
    }
}
