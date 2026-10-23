// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-image-build`: one image drive (§6 of docs/04-runtimes.md), started by a driver with its
//! request (`korpis_image::build::Request`) on standard input. It runs as two processes, so that
//! the one that reads what a publisher wrote holds no privilege.
//!
//! The first, root, resolves the image in the content store and makes a pid namespace and a work
//! directory owned by the request's ids. It starts the worker as that namespace's first process;
//! the worker drops its groups and makes its own user, network, and IPC namespaces, and says so.
//! The first maps the worker's ids from 0 to the request's 65,536, says so, and streams it each
//! layer, checked against its digest as it goes (`korpis_image::stream`). The worker becomes its
//! namespace's root, applies the layers, and runs `mke2fs` on what they made, in the same
//! namespace, so the drive's owners are the workload's ids from 0. Once the worker has exited 0,
//! the first makes the drive root's and read-only, moves it to where the request says, and
//! removes the work directory.
//!
//! This binary uses `unsafe` for `unshare`, which rustix marks unsafe for a flag it does not
//! pass; the call carries its `SAFETY` comment.

mod worker;

use korpis_image::build::{REQUEST_LIMIT, Request};
use korpis_image::{source, stream};
use rustix::thread::UnshareFlags;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};

/// The argument the worker is started with, before its work directory.
const WORKER: &str = "--worker";

/// The drive's name in the work directory, until it is moved.
const DRIVE: &str = "drive.ext4";

/// Unshares `flags` from this process, which is one thread.
fn unshare(flags: UnshareFlags) -> Result<(), String> {
    // SAFETY: `FILES` is never among the flags, so no descriptor table is unshared, and the
    // process is one thread whenever this is called.
    #[allow(unsafe_code, reason = "unshare is unsafe only with FILES, which is never asked for")]
    unsafe { rustix::thread::unshare_unsafe(flags) }.map_err(|e| format!("namespaces: {e}"))
}

/// One byte, the signal each process gives the other.
fn signal(to: &mut impl Write) -> Result<(), String> {
    to.write_all(b"!").and_then(|()| to.flush()).map_err(|e| format!("the other process: {e}"))
}

fn awaited(from: &mut impl Read) -> Result<(), String> {
    let mut byte = [0u8; 1];
    from.read_exact(&mut byte).map_err(|e| format!("the other process: {e}"))
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1);
    let outcome = match (args.next().as_deref(), args.next()) {
        (Some(WORKER), Some(dir)) => worker::main(Path::new(&dir)),
        _ => first(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "the image build failed");
            ExitCode::FAILURE
        }
    }
}

/// A work directory beside `out`, owned by `ids` and no one else.
fn work_dir(out: &Path, ids: u32) -> Result<PathBuf, String> {
    let parent = out.parent().ok_or("the drive's directory")?;
    let name = format!(".build-{}", std::process::id());
    let dir = parent.join(name);
    std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(|e| format!("{e}"))?;
    std::os::unix::fs::chown(&dir, Some(ids), Some(ids)).map_err(|e| format!("{e}"))?;
    Ok(dir)
}

fn map_ids(pid: u32, base: u32) -> Result<(), String> {
    let map = format!("0 {base} 65536\n");
    std::fs::write(format!("/proc/{pid}/setgroups"), "deny").map_err(|e| format!("{e}"))?;
    for file in ["uid_map", "gid_map"] {
        std::fs::write(format!("/proc/{pid}/{file}"), &map).map_err(|e| format!("{file}: {e}"))?;
    }
    Ok(())
}

fn spawn(dir: &Path) -> Result<Child, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    Command::new(exe)
        .arg(WORKER)
        .arg(dir)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .envs(std::env::var_os("RUST_LOG").map(|v| ("RUST_LOG", v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("the worker: {e}"))
}

/// Maps the worker's ids and streams it the layers of `source`.
fn feed(request: &Request, src: &source::Source, child: &mut Child) -> Result<(), String> {
    awaited(child.stdout.as_mut().ok_or("the worker's output")?)?;
    map_ids(child.id(), request.ids)?;
    let to = child.stdin.as_mut().ok_or("the worker's input")?;
    signal(to)?;
    for (layer, packing) in &src.layers {
        let path = source::blob(&request.content, &layer.digest).map_err(|e| e.to_string())?;
        stream::send(to, &path, (&layer.digest, layer.size, *packing))
            .map_err(|e| e.to_string())?;
    }
    stream::end(to).map_err(|e| e.to_string())
}

/// Builds in `dir` and, once the worker has made the drive, moves it to `request.out`.
fn build(request: &Request, src: &source::Source, dir: &Path) -> Result<(), String> {
    let mut child = spawn(dir)?;
    if let Err(e) = feed(request, src, &mut child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    drop(child.stdin.take());
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("the worker failed: {status}"));
    }
    let drive = dir.join(DRIVE);
    std::os::unix::fs::chown(&drive, Some(0), Some(0)).map_err(|e| format!("{e}"))?;
    std::fs::set_permissions(&drive, std::fs::Permissions::from_mode(0o444))
        .map_err(|e| format!("{e}"))?;
    std::fs::rename(&drive, &request.out).map_err(|e| format!("{}: {e}", request.out.display()))
}

/// The first process.
fn first() -> Result<(), String> {
    let mut body = Vec::new();
    std::io::stdin()
        .lock()
        .take(REQUEST_LIMIT)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    let request: Request =
        serde_json::from_slice(&body).map_err(|e| format!("the request: {e}"))?;
    let src = source::resolve(&request.content, &request.image).map_err(|e| e.to_string())?;
    unshare(UnshareFlags::NEWPID)?;
    let dir = work_dir(&request.out, request.ids)?;
    let built = build(&request, &src, &dir);
    // The work directory holds what a publisher's layers made: removed without following links.
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        tracing::warn!(error = %e, dir = %dir.display(), "the work directory was left");
    }
    built
}
