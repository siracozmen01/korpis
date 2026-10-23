// SPDX-License-Identifier: AGPL-3.0-or-later

//! A build as its caller asks for it: the image, where its drive goes, and the ids the worker's
//! user namespace maps from 0. The caller is a driver, which runs `korpis-image-build` with the
//! request on its standard input and finds the drive at `out` once it has exited 0; until then
//! nothing is at `out`, so a build that fails or is killed leaves no drive behind.

use serde::{Deserialize, Serialize};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The largest request read.
pub const REQUEST_LIMIT: u64 = 64 << 10;

/// What a build is asked to make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// The node's content store.
    pub content: PathBuf,
    /// The image, `name@sha256:...`.
    pub image: String,
    /// Where the drive goes: a file made only once the build has succeeded, read-only, owned
    /// by root.
    pub out: PathBuf,
    /// The first of the 65,536 host ids the worker's user namespace maps from 0, so the drive's
    /// files are owned by the workload's own ids; a range nothing else on the node runs as.
    pub ids: u32,
}

/// The name a drive is kept under: its image's manifest, which names its bytes.
#[must_use]
pub fn file_name(manifest: &str) -> String {
    format!("{}.ext4", manifest.replace(':', "-"))
}

/// Runs the build job `binary` for `request`, its log going where the caller's does.
pub fn run(binary: &Path, request: &Request) -> io::Result<()> {
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    let body = serde_json::to_vec(request).map_err(io::Error::other)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&body)?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other(format!("the image build failed: {status}")));
    }
    if !request.out.is_file() {
        return Err(io::Error::other("the image build made no drive"));
    }
    Ok(())
}
