// SPDX-License-Identifier: AGPL-3.0-or-later

//! File sessions in memory: each volume a session names is a directory of its own under the root
//! a test gives, served with the worker's own verbs on a thread, over a socket pair, so the
//! session's framing and answers are the worker's and only its confinement is missing.

use super::{HostError, InMemoryHost};
use korpis_files::volumes::Volumes;
use korpis_files::{frame, serve};
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use rustix::fs::{Mode, OFlags};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

fn failed(e: impl std::fmt::Display) -> HostError {
    HostError(e.to_string())
}

/// The directory `path`, made if it is not there, opened to be served.
fn dir(path: &Path) -> Result<OwnedFd, HostError> {
    std::fs::create_dir_all(path).map_err(failed)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    rustix::fs::open(path, flags, Mode::empty()).map_err(failed)
}

impl InMemoryHost {
    /// Serves the file sessions opened from now on from `root`: each volume in its directory
    /// named by the volume's identifier.
    pub fn serve_files_from(&self, root: &Path) {
        let _ = self.with(|k| k.file_root = Some(root.to_owned()));
    }

    /// The file sessions opened, in order.
    #[must_use]
    pub fn opened_files(&self) -> Vec<helper::OpenFilesRequest> {
        self.with(|k| k.opened_files.clone()).unwrap_or_default()
    }

    pub(super) fn serve_files(&self, r: &helper::OpenFilesRequest) -> Result<OwnedFd, HostError> {
        let root: Option<PathBuf> = self.with(|k| {
            k.opened_files.push(r.clone());
            k.file_root.clone()
        })?;
        let root = root.ok_or_else(|| failed("the in-memory host serves no files"))?;
        let top = root.join(".top");
        let mut volumes = Vec::with_capacity(r.volumes.len());
        for v in &r.volumes {
            dir(&top.join(v.path.trim_start_matches('/')))?;
            volumes.push((v.path.clone(), dir(&root.join(&v.volume))?, r.write && !v.read_only));
        }
        let served = Volumes::new(dir(&top)?, volumes);
        let (ours, theirs) = UnixStream::pair().map_err(failed)?;
        std::thread::spawn(move || answer(&served, theirs));
        Ok(OwnedFd::from(ours))
    }
}

/// Answers each request on `session` until it closes, as the worker does.
fn answer(volumes: &Volumes, mut session: UnixStream) {
    use buffa::Message as _;
    while let Ok(Some(bytes)) = frame::read(&mut session) {
        let Ok(request) = pb::FileRequest::decode_from_slice(&bytes) else { return };
        let answer = serve::answer(volumes, request).encode_to_vec();
        if frame::write(&mut session, &answer).is_err() {
            return;
        }
    }
}
