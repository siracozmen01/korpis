// SPDX-License-Identifier: AGPL-3.0-or-later

//! File sessions (§3.2 of docs/06-storage.md): a `korpis-fsworker` process serving one
//! session on a workload's volumes, as the workload's own ids, confined as an install's worker is.
//! The helper names each volume's directory from its identifier, checks each path the workload
//! sees it at, and refuses an id range its ledger does not record for the workload, as it does for
//! an install; it gives the worker its spec on a pipe and one end of a socket pair as its output,
//! and answers with the other end, so the session's bytes never pass through the helper. The
//! worker ends when that end is closed. Every worker runs in one cgroup beside the jobs', with a
//! job's limits.

use crate::error::{HelperError, HelperResult};
use crate::ids::Ledger;
use buffa::Message as _;
use korpis_proto::korpis::helper::v1 as helper;
use rustix::fd::OwnedFd;
use std::collections::BTreeSet;
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// Where file workers run from.
#[derive(Debug, Clone)]
pub struct FilesConfig {
    /// The `korpis-fsworker` binary.
    pub binary: PathBuf,
    /// The helper's state, where the empty directory each worker's root is built on is made.
    pub state: PathBuf,
    /// The cgroup the workers' is made in, as jobs' are; none to leave them in the helper's.
    pub cgroups: Option<PathBuf>,
}

/// The file workers this helper started.
#[derive(Debug)]
pub struct Files {
    config: FilesConfig,
    running: Vec<Child>,
}

/// The worker's spec for `r`, whose ids `ledger` must record for its workload, each volume's
/// directory found by `directory`.
pub(crate) fn place(
    ledger: &Ledger,
    r: &helper::OpenFilesRequest,
    mut directory: impl FnMut(&str) -> HelperResult<PathBuf>,
) -> HelperResult<helper::FileWorkerSpec> {
    crate::job::install::check_ids(ledger, &r.workload, r.id_base)?;
    if r.volumes.is_empty() {
        return Err(HelperError::Refused("a file session names its volumes".into()));
    }
    let mut seen = BTreeSet::new();
    let mut volumes = Vec::with_capacity(r.volumes.len());
    for v in &r.volumes {
        crate::job::install::check_path(&v.path)?;
        if !seen.insert(v.path.trim_end_matches('/')) {
            return Err(HelperError::Refused(format!("two volumes at {}", v.path)));
        }
        let dir = directory(&v.volume)?;
        volumes.push(helper::FileVolume {
            path: v.path.clone(),
            volume: v.volume.clone(),
            directory: dir.to_string_lossy().into_owned(),
            read_only: v.read_only,
            ..Default::default()
        });
    }
    Ok(helper::FileWorkerSpec { id_base: r.id_base, volumes, write: r.write, ..Default::default() })
}

impl Files {
    /// File workers as `config` says.
    #[must_use]
    pub fn new(config: FilesConfig) -> Self {
        Self { config, running: Vec::new() }
    }

    /// Whether a worker this helper started still runs; those that ended are reaped.
    pub fn running(&mut self) -> bool {
        self.running.retain_mut(|child| matches!(child.try_wait(), Ok(None)));
        !self.running.is_empty()
    }

    /// Starts a worker on `spec`; the session's end of its socket.
    ///
    /// # Errors
    /// The root's directory could not be made, or the worker could not be started.
    pub fn open(&mut self, mut spec: helper::FileWorkerSpec) -> HelperResult<OwnedFd> {
        self.running();
        // Each worker mounts its root on this directory in its own mount namespace, so one empty
        // directory serves them all and nothing is ever mounted on it in the host's.
        let root = self.config.state.join("files-root");
        std::fs::create_dir_all(&root)?;
        spec.root = root.to_string_lossy().into_owned();
        let cgroup = self.config.cgroups.as_deref().and_then(|b| crate::job::cgroup_in(b, "files"));
        spec.cgroup = cgroup.map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
        let (ours, theirs) = UnixStream::pair()?;
        let mut child = Command::new(&self.config.binary)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::from(OwnedFd::from(theirs)))
            .stderr(Stdio::inherit())
            .spawn()?;
        let written = match child.stdin.take() {
            Some(mut stdin) => stdin.write_all(&spec.encode_to_vec()),
            None => Err(std::io::Error::other("the worker's input")),
        };
        if let Err(e) = written {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.into());
        }
        tracing::info!(pid = child.id(), write = spec.write, "file worker started");
        self.running.push(child);
        Ok(OwnedFd::from(ours))
    }
}
