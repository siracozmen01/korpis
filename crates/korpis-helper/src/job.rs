// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backup and restore jobs (§5.5 of docs/06-storage.md), and install jobs (§4 of
//! docs/09-recipes.md, in [`install`]): each a process the helper starts and then only watches. The helper names every path the job uses from identifiers
//! it checks; the job receives its spec, the data key and the store's access with it, on its
//! standard input, and the helper never writes the spec anywhere. Each job has a directory in the
//! helper's state for its result and its log, and a cgroup of its own beside the workloads', with
//! a low weight and a ceiling, which the job joins before it does anything. A job outlives a
//! helper that exits: a restarted helper finds it by the process its pid file names, if that
//! process is still the job binary.

use crate::error::{HelperError, HelperResult, identifier};
use crate::kept::Kept;
use buffa::Message as _;
use korpis_proto::korpis::backup::v1 as pb;
use korpis_proto::korpis::helper::v1 as helper;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub mod install;
mod into;

pub use into::make_into;

/// A job's cgroup limits: a tenth of a workload's default weight, so it yields under contention
/// and runs freely otherwise, and ceilings on its memory and processes.
const LIMITS: &[(&str, &str)] = &[
    ("cpu.weight", "10"),
    ("io.weight", "default 10"),
    ("memory.max", "1073741824"),
    ("pids.max", "1024"),
];

/// Where jobs run from and keep their state.
#[derive(Debug, Clone)]
pub struct JobConfig {
    /// The `korpis-backup-job` binary.
    pub binary: PathBuf,
    /// The `korpis-install-job` binary; none to run no install jobs.
    pub install_binary: Option<PathBuf>,
    /// Where each job's directory is made.
    pub state: PathBuf,
    /// The cgroup jobs' cgroups are made in; none to run them in the helper's.
    pub cgroups: Option<PathBuf>,
    /// The content store's directory: a restore reads recipe content from its `blobs/sha256`,
    /// and an install reads the recipe's files and what its fetches downloaded from it.
    pub content: Option<PathBuf>,
}

/// The cgroup `name` beneath `base`, made with a job's limits, or none where none can be made.
pub(crate) fn cgroup_in(base: &Path, name: &str) -> Option<PathBuf> {
    if let Err(e) = std::fs::create_dir_all(base) {
        tracing::warn!(error = %e, "no cgroup for jobs");
        return None;
    }
    for controller in ["+cpu", "+io", "+memory", "+pids"] {
        let _ = std::fs::write(base.join("cgroup.subtree_control"), controller);
    }
    let dir = base.join(name);
    match std::fs::create_dir(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            tracing::warn!(error = %e, %name, "no cgroup");
            return None;
        }
    }
    for (file, value) in LIMITS {
        // A controller the host has not enabled has no file; its limit is then not applied.
        if let Err(e) = std::fs::write(dir.join(file), value) {
            tracing::debug!(error = %e, %file, "a job limit not applied");
        }
    }
    Some(dir)
}

/// The jobs this helper started, and how to start more.
#[derive(Debug)]
pub struct Jobs {
    config: JobConfig,
    running: BTreeMap<String, Child>,
}

/// What a job works in, as the helper resolved it.
#[derive(Debug)]
pub enum Placed {
    /// A backup or a restore.
    Data(DataJob),
    /// An install.
    Install(Box<korpis_proto::korpis::install::v1::InstallSpec>),
}

/// A backup or restore job, placed.
#[derive(Debug)]
pub struct DataJob {
    /// The directory read or written.
    pub directory: PathBuf,
    /// The work, as the job's spec carries it.
    pub work: pb::__buffa::oneof::job_spec::Work,
    /// What it leaves on a volume until it is forgotten.
    pub kept: Option<Kept>,
}

impl crate::storage::Class {
    /// What a backup of `volume` reads for job `job`: the snapshot `snapshot` of it; one taken
    /// for the job when `take`, which is then also returned to be released; or the live volume.
    pub fn backup_source(
        &mut self,
        (volume, snapshot): (&str, &str),
        (job, take): (&str, bool),
    ) -> HelperResult<(PathBuf, Option<(String, String)>)> {
        if snapshot.is_empty() && take {
            let name = format!("job-{job}");
            self.snapshot(volume, &name)?;
            let taken = self.snapshot_dir(volume).join(&name);
            return Ok((taken, Some((volume.to_owned(), name))));
        }
        Ok((self.existing_source(volume, snapshot)?, None))
    }

    fn existing_source(&self, volume: &str, snapshot: &str) -> HelperResult<PathBuf> {
        if snapshot.is_empty() {
            return Ok(self.path_of(volume));
        }
        identifier("snapshot", snapshot)?;
        let taken = self.snapshot_dir(volume).join(snapshot);
        if taken.is_dir() {
            Ok(taken)
        } else {
            Err(HelperError::NotFound(format!("snapshot {snapshot} of volume {volume}")))
        }
    }
}

impl Jobs {
    /// Jobs as `config` says.
    #[must_use]
    pub fn new(config: JobConfig) -> Self {
        Self { config, running: BTreeMap::new() }
    }

    fn dir(&self, job: &str) -> PathBuf {
        self.config.state.join("jobs").join(job)
    }

    /// Whether any job this helper started is still running.
    pub fn busy(&mut self) -> bool {
        self.running.retain(|_, child| matches!(child.try_wait(), Ok(None)));
        !self.running.is_empty()
    }

    /// Starts job `r.job` working in `placed`; whether it was started now.
    pub fn start(&mut self, r: &helper::StartJobRequest, placed: Placed) -> HelperResult<bool> {
        identifier("job", &r.job)?;
        let dir = self.dir(&r.job);
        if self.running.contains_key(&r.job) || dir.exists() {
            return Ok(false);
        }
        let binary =
            match &placed {
                Placed::Data(_) => self.config.binary.clone(),
                Placed::Install(_) => self.config.install_binary.clone().ok_or_else(|| {
                    HelperError::Refused("this helper runs no install jobs".into())
                })?,
            };
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let cgroup = self.cgroup(&r.job);
        let spec = match placed {
            Placed::Data(d) => self.data_spec(r, d, &dir, cgroup.as_deref())?,
            Placed::Install(spec) => {
                let bounds = (cgroup.as_deref(), self.config.content.as_deref());
                install::finish(*spec, &r.job, &dir, bounds)?
            }
        };
        let log = std::fs::File::create(dir.join("log"))?;
        let mut child = Command::new(&binary)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        std::fs::write(dir.join("pid"), child.id().to_string())?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&spec)?;
        }
        tracing::info!(job = %r.job, "job started");
        self.running.insert(r.job.clone(), child);
        Ok(true)
    }

    /// The spec of a backup or restore job, whose directory is `dir`.
    fn data_spec(
        &self,
        r: &helper::StartJobRequest,
        placed: DataJob,
        dir: &Path,
        cgroup: Option<&Path>,
    ) -> HelperResult<Vec<u8>> {
        let string = |p: &Path| p.to_string_lossy().into_owned();
        let spec = pb::JobSpec {
            id: r.job.clone(),
            work: Some(placed.work),
            data_key: r.data_key.clone(),
            store: r.store.clone(),
            content_store: r.content_store.clone(),
            concurrency: r.concurrency,
            directory: string(&placed.directory),
            cgroup: cgroup.map(string).unwrap_or_default(),
            result: string(&dir.join("result")),
            // The content store's root: recipe files and what fetches downloaded, by sha256.
            content: self.config.content.as_deref().map(string).unwrap_or_default(),
            ..Default::default()
        };
        if let Some(kept) = &placed.kept {
            std::fs::write(dir.join("kept"), kept.to_line())?;
        }
        Ok(spec.encode_to_vec())
    }

    /// The cgroup made for `job`, or none where none can be made, which the job then reports.
    fn cgroup(&self, job: &str) -> Option<PathBuf> {
        cgroup_in(self.config.cgroups.as_ref()?, &format!("job-{job}"))
    }

    /// The pid of `job` if a process of its job binary has it.
    fn alive(&self, job: &str) -> Option<i32> {
        let dir = self.dir(job);
        let pid: i32 = std::fs::read_to_string(dir.join("pid")).ok()?.trim().parse().ok()?;
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        let binary = if install::is_install(&dir) {
            self.config.install_binary.as_ref()?
        } else {
            &self.config.binary
        };
        let ours = std::fs::canonicalize(binary).ok()?;
        (exe == ours).then_some(pid)
    }

    /// Where `job` is.
    pub fn status(&mut self, job: &str) -> HelperResult<helper::JobStatusResponse> {
        identifier("job", job)?;
        let state = |s: helper::JobState| helper::JobStatusResponse {
            state: s.into(),
            ..Default::default()
        };
        let dir = self.dir(job);
        if install::is_install(&dir) {
            return self.install_status(job, &dir);
        }
        if let Some(child) = self.running.get_mut(job) {
            if child.try_wait()?.is_none() {
                return Ok(state(helper::JobState::JOB_STATE_RUNNING));
            }
            self.running.remove(job);
        }
        match std::fs::read(dir.join("result")) {
            Ok(bytes) => {
                let result = pb::JobResult::decode_from_slice(&bytes)
                    .map_err(|e| HelperError::Failed(format!("job {job}'s result: {e}")))?;
                let mut s = state(helper::JobState::JOB_STATE_FINISHED);
                s.result = result.into();
                return Ok(s);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        if !dir.exists() {
            return Ok(state(helper::JobState::JOB_STATE_UNKNOWN));
        }
        Ok(state(if self.alive(job).is_some() {
            helper::JobState::JOB_STATE_RUNNING
        } else {
            helper::JobState::JOB_STATE_LOST
        }))
    }

    /// Where the install job `job`, whose directory is `dir`, is, and how far it has come.
    fn install_status(&mut self, job: &str, dir: &Path) -> HelperResult<helper::JobStatusResponse> {
        let running = match self.running.get_mut(job) {
            Some(child) => child.try_wait()?.is_none(),
            None => self.alive(job).is_some(),
        };
        if !running {
            self.running.remove(job);
        }
        let (result, progress) = install::recorded(dir)?;
        let state = match (running, &result) {
            (true, _) => helper::JobState::JOB_STATE_RUNNING,
            (false, Some(_)) => helper::JobState::JOB_STATE_FINISHED,
            (false, None) => helper::JobState::JOB_STATE_LOST,
        };
        Ok(helper::JobStatusResponse {
            state: state.into(),
            install: result.into(),
            progress,
            ..Default::default()
        })
    }

    /// Ends `job` if it runs; whether it did.
    pub fn cancel(&mut self, job: &str) -> HelperResult<bool> {
        identifier("job", job)?;
        if let Some(mut child) = self.running.remove(job) {
            let running = child.try_wait()?.is_none();
            if running {
                child.kill()?;
            }
            child.wait()?;
            return Ok(running);
        }
        let Some(pid) = self.alive(job) else { return Ok(false) };
        let pid = rustix::process::Pid::from_raw(pid)
            .ok_or_else(|| HelperError::Failed("a pid".into()))?;
        rustix::process::kill_process(pid, rustix::process::Signal::KILL)
            .map_err(std::io::Error::from)?;
        Ok(true)
    }

    /// What `job` keeps on a volume, if anything.
    pub fn kept_of(&self, job: &str) -> HelperResult<Option<Kept>> {
        identifier("job", job)?;
        match std::fs::read_to_string(self.dir(job).join("kept")) {
            Ok(line) => Kept::from_line(line.trim_end()).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Whether `job` was started and not forgotten.
    pub fn known(&self, job: &str) -> HelperResult<bool> {
        identifier("job", job)?;
        Ok(self.running.contains_key(job) || self.dir(job).exists())
    }

    /// Every job started and not forgotten, and where each is.
    pub fn list(&mut self) -> HelperResult<Vec<helper::JobSummary>> {
        let entries = match std::fs::read_dir(self.config.state.join("jobs")) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut jobs = Vec::new();
        for entry in entries {
            let Some(job) = entry?.file_name().to_str().map(str::to_owned) else { continue };
            let state = self.status(&job)?.state;
            jobs.push(helper::JobSummary { job, state, ..Default::default() });
        }
        jobs.sort_by(|a, b| a.job.cmp(&b.job));
        Ok(jobs)
    }

    /// Removes what is kept of `job`, which must have ended.
    pub fn forget(&mut self, job: &str) -> HelperResult<()> {
        identifier("job", job)?;
        if self.status(job)?.state.as_known() == Some(helper::JobState::JOB_STATE_RUNNING) {
            return Err(HelperError::Refused(format!("job {job} is running")));
        }
        match std::fs::remove_dir_all(self.dir(job)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        if let Some(base) = &self.config.cgroups {
            let _ = std::fs::remove_dir(base.join(format!("job-{job}")));
        }
        Ok(())
    }
}
