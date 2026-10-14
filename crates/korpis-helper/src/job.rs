// SPDX-License-Identifier: AGPL-3.0-or-later

//! Backup and restore jobs (§5.5 of docs/06-storage.md): each a `korpis-backup-job` process the
//! helper starts and then only watches. The helper names every path the job uses from identifiers
//! it checks; the job receives its spec, the data key and the store's access with it, on its
//! standard input, and the helper never writes the spec anywhere. Each job has a directory in the
//! helper's state for its result and its log, and a cgroup of its own beside the workloads', with
//! a low weight and a ceiling, which the job joins before it does anything. A job outlives a
//! helper that exits: a restarted helper finds it by the process its pid file names, if that
//! process is still the job binary.

use crate::error::{HelperError, HelperResult, identifier};
use buffa::Message as _;
use korpis_proto::korpis::backup::v1 as pb;
use korpis_proto::korpis::helper::v1 as helper;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

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
    /// Where each job's directory is made.
    pub state: PathBuf,
    /// The cgroup jobs' cgroups are made in; none to run them in the helper's.
    pub cgroups: Option<PathBuf>,
    /// The content store's blobs, which a restore reads recipe content from.
    pub content: Option<PathBuf>,
}

/// The jobs this helper started, and how to start more.
#[derive(Debug)]
pub struct Jobs {
    config: JobConfig,
    running: BTreeMap<String, Child>,
}

/// What a job works in, as the helper resolved it.
#[derive(Debug)]
pub struct Placed {
    /// The directory read or written.
    pub directory: PathBuf,
    /// The work, as the job's spec carries it.
    pub work: pb::__buffa::oneof::job_spec::Work,
    /// A snapshot taken for the job, by volume and name, which forgetting it releases.
    pub release: Option<(String, String)>,
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
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        let cgroup = self.cgroup(&r.job);
        let spec = pb::JobSpec {
            id: r.job.clone(),
            work: Some(placed.work),
            data_key: r.data_key.clone(),
            store: r.store.clone(),
            concurrency: r.concurrency,
            directory: placed.directory.to_string_lossy().into_owned(),
            cgroup: cgroup.map(|c| c.to_string_lossy().into_owned()).unwrap_or_default(),
            result: dir.join("result").to_string_lossy().into_owned(),
            content: self
                .config
                .content
                .as_ref()
                .map(|c| c.to_string_lossy().into_owned())
                .unwrap_or_default(),
            ..Default::default()
        };
        if let Some((volume, snapshot)) = &placed.release {
            std::fs::write(dir.join("snapshot"), format!("{volume}\n{snapshot}"))?;
        }
        let log = std::fs::File::create(dir.join("log"))?;
        let mut child = Command::new(&self.config.binary)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        std::fs::write(dir.join("pid"), child.id().to_string())?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&spec.encode_to_vec())?;
        }
        tracing::info!(job = %r.job, "job started");
        self.running.insert(r.job.clone(), child);
        Ok(true)
    }

    /// The cgroup made for `job`, or none where none can be made, which the job then reports.
    fn cgroup(&self, job: &str) -> Option<PathBuf> {
        let base = self.config.cgroups.as_ref()?;
        if let Err(e) = std::fs::create_dir_all(base) {
            tracing::warn!(error = %e, "no cgroup for jobs");
            return None;
        }
        for controller in ["+cpu", "+io", "+memory", "+pids"] {
            let _ = std::fs::write(base.join("cgroup.subtree_control"), controller);
        }
        let dir = base.join(format!("job-{job}"));
        match std::fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                tracing::warn!(error = %e, %job, "no cgroup for the job");
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

    /// The pid of `job` if a process of the job binary has it.
    fn alive(&self, job: &str) -> Option<i32> {
        let pid: i32 =
            std::fs::read_to_string(self.dir(job).join("pid")).ok()?.trim().parse().ok()?;
        let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        let ours = std::fs::canonicalize(&self.config.binary).ok()?;
        (exe == ours).then_some(pid)
    }

    /// Where `job` is.
    pub fn status(&mut self, job: &str) -> HelperResult<helper::JobStatusResponse> {
        identifier("job", job)?;
        let state = |s: helper::JobState| helper::JobStatusResponse {
            state: s.into(),
            ..Default::default()
        };
        if let Some(child) = self.running.get_mut(job) {
            if child.try_wait()?.is_none() {
                return Ok(state(helper::JobState::JOB_STATE_RUNNING));
            }
            self.running.remove(job);
        }
        let dir = self.dir(job);
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

    /// The snapshot taken for `job`, by volume and name, if one was.
    pub fn release_of(&self, job: &str) -> HelperResult<Option<(String, String)>> {
        identifier("job", job)?;
        match std::fs::read_to_string(self.dir(job).join("snapshot")) {
            Ok(s) => Ok(s.split_once('\n').map(|(v, n)| (v.to_owned(), n.to_owned()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
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

/// Makes `into` at the root of the volume at `volume`, owned and permitted as the root is, so the
/// workload reaches what is restored there; it must not exist.
pub fn make_into(volume: &Path, into: &str) -> HelperResult<PathBuf> {
    use rustix::fs::{Mode, OFlags};
    identifier("restore directory", into)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root = rustix::fs::open(volume, flags, Mode::empty()).map_err(std::io::Error::from)?;
    let st = rustix::fs::fstat(&root).map_err(std::io::Error::from)?;
    match rustix::fs::mkdirat(&root, into, Mode::from_raw_mode(0o700)) {
        Ok(()) => {}
        Err(rustix::io::Errno::EXIST) => {
            return Err(HelperError::Refused(format!("{into} already exists in the volume")));
        }
        Err(e) => return Err(std::io::Error::from(e).into()),
    }
    // Through a descriptor of what was made: a running workload may put a link in its place.
    let made =
        rustix::fs::openat(&root, into, flags, Mode::empty()).map_err(std::io::Error::from)?;
    let (uid, gid) = (
        rustix::fs::Uid::from_raw_unchecked(st.st_uid),
        rustix::fs::Gid::from_raw_unchecked(st.st_gid),
    );
    rustix::fs::fchown(&made, Some(uid), Some(gid)).map_err(std::io::Error::from)?;
    rustix::fs::fchmod(&made, Mode::from_raw_mode(st.st_mode & 0o7777))
        .map_err(std::io::Error::from)?;
    Ok(volume.join(into))
}
