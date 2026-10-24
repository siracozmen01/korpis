// SPDX-License-Identifier: AGPL-3.0-or-later

//! An install in a service guest (§4.4 of docs/04-runtimes.md, §4 of docs/09-recipes.md): the
//! install job the boot drive carries, run as the node's helper runs it beside a container, its
//! spec the content drive's `spec`, which the driver wrote naming the guest's own paths: the
//! volumes where this guest mounts them, the content drive where it is mounted, its root and its
//! output in the guest's `/run`, and the guest's own ids from 0. It makes its own namespaces,
//! confines itself, runs the actions, and writes its progress and result in its output, which the
//! agent reads back for the driver. The guest runs nothing else, and stays up once the job has
//! ended, serving file sessions, until the driver stops it.

use korpis_guest::control::Installing;
use korpis_guest::drive::{INSTALL_CONTENT, INSTALL_OUT, INSTALL_ROOT, INSTALL_SPEC};
use std::fs::File;
use std::io::Read as _;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};

/// The most of a result read back, leaving the frame its fixed part.
const MOST_RESULT: u64 = 60 * 1024;
/// The largest spec given in the config: what a pipe holds unread.
const MOST_GIVEN: usize = 60 * 1024;

/// The install job's process id once started, zero before, below zero if it could not start.
static JOB: AtomicI32 = AtomicI32::new(0);

/// Starts the install job at `job`, the boot drive's binary held open, with `spec` as its input,
/// or else the content drive's. One that cannot be started is said on the console and reported as
/// ended.
pub fn start(job: &File, spec: Option<&[u8]>) {
    if let Err(e) = started(job, spec) {
        crate::say(&format!("the install job: {e}"));
        start_failed();
    }
}

/// Says the install job could not be started, so the driver hears it has ended with no result.
pub fn start_failed() {
    JOB.store(-1, Ordering::Release);
}

fn started(job: &File, given: Option<&[u8]>) -> std::io::Result<()> {
    for dir in [INSTALL_ROOT, INSTALL_OUT] {
        std::fs::DirBuilder::new().mode(0o700).recursive(true).create(dir)?;
    }
    let input = match given {
        // Written into a pipe before the job reads it, so no larger than a pipe holds.
        Some(bytes) if bytes.len() > MOST_GIVEN => {
            return Err(std::io::Error::other("a job's spec larger than a pipe holds"));
        }
        Some(bytes) => {
            let (read, write) = rustix::pipe::pipe()?;
            std::io::Write::write_all(&mut File::from(write), bytes)?;
            Stdio::from(read)
        }
        None => Stdio::from(File::open(Path::new(INSTALL_CONTENT).join(INSTALL_SPEC))?),
    };
    let program = format!("/proc/self/fd/{}", job.as_raw_fd());
    let pid = crate::exec::tracked(|| {
        Command::new(&program)
            .env_clear()
            .env("RUST_LOG", "info")
            .stdin(input)
            .stdout(Stdio::null())
            .spawn()
    })?;
    JOB.store(pid, Ordering::Release);
    Ok(())
}

/// How the install goes: what its output says, and whether its job has ended.
pub fn status() -> Installing {
    let pid = JOB.load(Ordering::Acquire);
    let ended = pid < 0 || (pid > 0 && crate::exec::status_of(pid).is_some());
    let out = Path::new(INSTALL_OUT);
    let progress = std::fs::read_to_string(out.join("progress"))
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0);
    let mut result = Vec::new();
    if let Ok(f) = File::open(out.join("result")) {
        let _ = f.take(MOST_RESULT).read_to_end(&mut result);
    }
    Installing { progress, ended, result }
}
