// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-fsworker`: one file session (§3.2 of docs/06-storage.md), started by the node's
//! helper with its spec on standard input and the session's socket as its standard output. It
//! runs as two processes, as the install job does (§4 of docs/09-recipes.md), so that the one that
//! reads what a tenant sends holds no privilege beyond its workload's files.
//!
//! The first, root, joins the cgroup the spec names, makes a pid namespace, and starts the worker
//! in it as that namespace's first process, with the session as its standard input and a socket
//! to the first as its standard output. The worker drops its supplementary groups and makes its
//! own user, mount, network, and IPC namespaces, then says so. The first maps the worker's ids to
//! its workload's and builds its root, the workload's volumes at their paths and nothing else
//! (`korpis_confine::root`), then says so. The worker becomes its user namespace's root, confines
//! itself further (`confine`), and answers the session's requests, one frame each way
//! (`korpis_files::frame`), until the session's other end is closed.
//!
//! This binary uses `unsafe` for `unshare`, which rustix marks unsafe for a flag it does not
//! pass; the call carries its `SAFETY` comment.

mod confine;

use buffa::Message as _;
use korpis_files::{frame, serve, volumes::Volumes};
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use rustix::thread::UnshareFlags;
use std::io::Read as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, ExitCode, Stdio};

/// The largest spec read.
const SPEC_LIMIT: u64 = 1 << 20;

/// The argument the worker is started with.
const WORKER: &str = "--worker";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let worker = std::env::args().nth(1).as_deref() == Some(WORKER);
    let outcome = if worker { worker_main() } else { first() };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "the file session failed");
            ExitCode::FAILURE
        }
    }
}

/// Unshares `flags` from this process, which is one thread.
fn unshare(flags: UnshareFlags) -> Result<(), String> {
    // SAFETY: `FILES` is never among the flags, so no descriptor table is unshared, and the
    // process is one thread whenever this is called.
    #[allow(unsafe_code, reason = "unshare is unsafe only with FILES, which is never asked for")]
    unsafe { rustix::thread::unshare_unsafe(flags) }.map_err(|e| format!("namespaces: {e}"))
}

/// One frame, the signal each process gives the other.
fn signal(to: &mut UnixStream) -> Result<(), String> {
    frame::write(to, b"!").map_err(|e| format!("the other process: {e}"))
}

fn awaited(from: &mut UnixStream) -> Result<Vec<u8>, String> {
    frame::read(from)
        .map_err(|e| format!("the other process: {e}"))?
        .ok_or_else(|| "the other process ended".to_owned())
}

fn decode(bytes: &[u8]) -> Result<helper::FileWorkerSpec, String> {
    helper::FileWorkerSpec::decode_from_slice(bytes).map_err(|e| format!("the spec: {e}"))
}

/// The root `spec` asks for, as the confinement builds it.
fn root_of(spec: &helper::FileWorkerSpec) -> korpis_confine::root::Spec {
    let volumes = spec.volumes.iter().map(|v| korpis_confine::root::Volume {
        path: v.path.clone(),
        directory: v.directory.clone(),
    });
    korpis_confine::root::Spec {
        root: spec.root.clone(),
        id_base: spec.id_base,
        volumes: volumes.collect(),
        content: String::new(),
    }
}

/// The first process.
fn first() -> Result<(), String> {
    let mut bytes = Vec::new();
    std::io::stdin().lock().take(SPEC_LIMIT).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let spec = decode(&bytes)?;
    if !spec.cgroup.is_empty() {
        korpis_confine::join_cgroup(std::path::Path::new(&spec.cgroup))
            .map_err(|e| e.to_string())?;
    }
    unshare(UnshareFlags::NEWPID)?;
    let (mut control, theirs) = UnixStream::pair().map_err(|e| e.to_string())?;
    let mut child = spawn(theirs)?;
    if let Err(e) = prepare(&spec, &bytes, &mut control, &child) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("the worker ended: {status}")) }
}

/// Starts the worker on the session, with `control` to this process as its standard output.
fn spawn(control: UnixStream) -> Result<Child, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let session = std::io::stdout().as_fd().try_clone_to_owned().map_err(|e| e.to_string())?;
    Command::new(exe)
        .arg(WORKER)
        .env_clear()
        .envs(std::env::var_os("RUST_LOG").map(|v| ("RUST_LOG", v)))
        .stdin(Stdio::from(session))
        .stdout(Stdio::from(OwnedFd::from(control)))
        .spawn()
        .map_err(|e| format!("the worker: {e}"))
}

/// The spec sent; once the worker has its namespaces, its ids and root built, and the worker
/// told.
fn prepare(
    spec: &helper::FileWorkerSpec,
    bytes: &[u8],
    control: &mut UnixStream,
    child: &Child,
) -> Result<(), String> {
    frame::write(control, bytes).map_err(|e| format!("the worker: {e}"))?;
    awaited(control)?;
    korpis_confine::root::build(&root_of(spec), child.id())?;
    signal(control)
}

/// The worker.
fn worker_main() -> Result<(), String> {
    // It dies with the first process, which is the one the helper started and watches.
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
        .map_err(|e| e.to_string())?;
    let stdout = std::io::stdout().as_fd().try_clone_to_owned().map_err(|e| e.to_string())?;
    let mut control = UnixStream::from(stdout);
    let spec = decode(&awaited(&mut control)?)?;
    // Groups are kept across a user namespace whether it maps them or not: dropped first.
    rustix::thread::set_thread_groups(&[]).map_err(|e| format!("groups: {e}"))?;
    let flags =
        UnshareFlags::NEWUSER | UnshareFlags::NEWNS | UnshareFlags::NEWNET | UnshareFlags::NEWIPC;
    unshare(flags)?;
    signal(&mut control)?;
    awaited(&mut control)?;
    let paths: Vec<&str> = spec.volumes.iter().map(|v| v.path.as_str()).collect();
    let mut opened = Vec::with_capacity(paths.len());
    for p in &paths {
        opened.push(((*p).to_owned(), confine::dir(p).map_err(|e| e.to_string())?));
    }
    let top = confine::dir("/").map_err(|e| e.to_string())?;
    let held = confine::confine(&paths, spec.write).map_err(|e| e.to_string())?;
    if !held.landlock {
        tracing::warn!("Landlock is not enforced by this kernel: confined by the root alone");
    }
    tracing::info!(write = spec.write, capabilities = ?held.capabilities, "file session open");
    serve_session(&Volumes::new(top, opened, spec.write))
}

/// Answers the session's requests until its other end is closed.
fn serve_session(volumes: &Volumes) -> Result<(), String> {
    let stdin = std::io::stdin().as_fd().try_clone_to_owned().map_err(|e| e.to_string())?;
    let mut session = std::fs::File::from(stdin);
    let mut answered = 0u64;
    while let Some(bytes) = frame::read(&mut session).map_err(|e| format!("the session: {e}"))? {
        let request = pb::FileRequest::decode_from_slice(&bytes)
            .map_err(|e| format!("a request that does not decode: {e}"))?;
        let answer = serve::answer(volumes, request).encode_to_vec();
        frame::write(&mut session, &answer).map_err(|e| format!("the session: {e}"))?;
        answered += 1;
    }
    tracing::info!(answered, "file session ended");
    Ok(())
}
