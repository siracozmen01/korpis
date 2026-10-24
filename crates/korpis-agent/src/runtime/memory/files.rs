// SPDX-License-Identifier: AGPL-3.0-or-later

//! The in-memory runtime's file sessions inside its objects, as a driver with a guest worker
//! serves them (§4.4 of docs/04-runtimes.md): where a test has given its objects a directory,
//! each session is answered by the file worker's own verbs over that directory, its `data`
//! directory served as a volume at `/data`, writing if the session does.

use super::{InMemory, State};
use crate::runtime::{RuntimeError, RuntimeResult};
use buffa::Message as _;
use korpis_files::frame::LIMIT;
use korpis_files::volumes::Volumes;
use korpis_proto::korpis::driver::v1 as driver;
use korpis_proto::korpis::v1 as pb;
use rustix::fs::{Mode, OFlags};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};

impl InMemory {
    /// Serves its objects' file sessions inside them over `dir`, or leaves them to the host
    /// again.
    pub fn guest_files(&self, dir: Option<PathBuf>) {
        if let Ok(mut s) = self.lock() {
            s.guest_files = dir;
        }
    }

    pub(super) fn serves_now(&self, id: &str) -> RuntimeResult<bool> {
        Ok(self.call("serves_files", id)?.guest_files.is_some())
    }

    pub(super) fn files_now(&self, id: &str, write: bool) -> RuntimeResult<DuplexStream> {
        let s = self.call("files", id)?;
        let dir =
            running(&s, id)?.ok_or_else(|| RuntimeError::Failed(format!("{id} has no worker")))?;
        drop(s);
        let volumes = volumes(&dir, write).map_err(|e| RuntimeError::Failed(e.to_string()))?;
        let (ours, theirs) = tokio::io::duplex(2 * LIMIT);
        tokio::spawn(serve(theirs, volumes));
        Ok(ours)
    }
}

/// The directory `id` is served over, if it runs.
fn running(s: &State, id: &str) -> RuntimeResult<Option<PathBuf>> {
    let o = s.objects.get(id).ok_or_else(|| RuntimeError::NotFound(id.into()))?;
    if o.status.state.as_known() != Some(driver::RuntimeState::RUNTIME_STATE_RUNNING) {
        return Err(RuntimeError::Failed(format!("{id} does not run")));
    }
    Ok(s.guest_files.clone())
}

fn volumes(dir: &Path, write: bool) -> rustix::io::Result<Volumes> {
    let open = |p: &Path| {
        rustix::fs::open(p, OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())
    };
    Ok(Volumes::new(open(dir)?, vec![("/data".into(), open(&dir.join("data"))?, write)]))
}

/// Answers each request framed on `session` until it ends.
async fn serve(mut session: DuplexStream, volumes: Volumes) {
    loop {
        let mut length = [0_u8; 4];
        if session.read_exact(&mut length).await.is_err() {
            return;
        }
        let length = usize::try_from(u32::from_le_bytes(length)).unwrap_or(usize::MAX);
        if length > LIMIT {
            return;
        }
        let mut bytes = vec![0; length];
        if session.read_exact(&mut bytes).await.is_err() {
            return;
        }
        let Ok(request) = pb::FileRequest::decode_from_slice(&bytes) else { return };
        let answer = korpis_files::serve::answer(&volumes, request).encode_to_vec();
        let Ok(length) = u32::try_from(answer.len()) else { return };
        if session.write_all(&length.to_le_bytes()).await.is_err()
            || session.write_all(&answer).await.is_err()
        {
            return;
        }
    }
}
