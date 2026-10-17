// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-install-job`: one install (§4 of docs/09-recipes.md), started by the node's helper with
//! its spec on standard input. It runs as two processes, so that the one that reads what a recipe
//! supplies holds no privilege (§6 of docs/02-architecture.md).
//!
//! The first, root, joins the job's cgroup, makes a pid namespace, and starts the worker in it,
//! as that namespace's first process: it can name, signal, or trace no process outside it, and
//! dies with the first. The worker drops its supplementary groups and makes its own user, mount,
//! network, and IPC namespaces, then says so on its standard output. The first maps the worker's
//! ids to its workload's and builds its root (`setup`), then says so on the worker's standard
//! input. The worker becomes its user namespace's root, which is the workload's root and no one
//! else, confines itself further (`confine`), runs the install (`work`), and writes its result in
//! its output directory. Each exits 0 when a result was written, whatever it says.
//!
//! This binary uses `unsafe` for `unshare`, which rustix marks unsafe for a flag it does not
//! pass, and for `mount_setattr`, which rustix does not bind; each call carries its `SAFETY`
//! comment.

mod confine;
mod work;

use buffa::Message as _;
use korpis_proto::korpis::install::v1 as pb;
use rustix::fs::{Mode, OFlags};
use rustix::thread::UnshareFlags;
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::process::{Child, Command, ExitCode, Stdio};

/// The largest spec read.
const SPEC_LIMIT: u64 = 64 << 20;

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
    outcome.unwrap_or_else(|e| {
        tracing::error!(error = %e, "the install job failed");
        ExitCode::FAILURE
    })
}

/// Unshares `flags` from this process, which is one thread.
fn unshare(flags: UnshareFlags) -> Result<(), String> {
    // SAFETY: `FILES` is never among the flags, so no descriptor table is unshared, and the
    // process is one thread whenever this is called.
    #[allow(unsafe_code, reason = "unshare is unsafe only with FILES, which is never asked for")]
    unsafe { rustix::thread::unshare_unsafe(flags) }.map_err(|e| format!("namespaces: {e}"))
}

/// One byte, the signal each process gives the other.
fn signal(to: &mut impl Write) -> std::io::Result<()> {
    to.write_all(b"!")?;
    to.flush()
}

fn awaited(from: &mut impl Read) -> Result<(), String> {
    let mut byte = [0u8; 1];
    from.read_exact(&mut byte).map_err(|e| format!("the other process: {e}"))
}

/// The output directory, opened as root before anything else.
fn out_of(spec: &pb::InstallSpec) -> Result<OwnedFd, String> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    rustix::fs::open(spec.out.as_str(), flags, Mode::empty())
        .map_err(|e| format!("the output directory: {e}"))
}

/// The first process.
fn first() -> Result<ExitCode, String> {
    let mut bytes = Vec::new();
    std::io::stdin().lock().take(SPEC_LIMIT).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let spec = pb::InstallSpec::decode_from_slice(&bytes).map_err(|e| format!("the spec: {e}"))?;
    let out = out_of(&spec)?;
    let failed = |e: String| {
        let result = pb::InstallResult { error: e, done: spec.from, ..Default::default() };
        work::record(&out, "result", &result.encode_to_vec()).map_err(|e| e.to_string())?;
        Ok(ExitCode::SUCCESS)
    };
    if !spec.cgroup.is_empty() {
        let joined = korpis_confine::join_cgroup(std::path::Path::new(&spec.cgroup));
        if let Err(e) = joined {
            return failed(e.to_string());
        }
    }
    let mut child = match unshare(UnshareFlags::NEWPID).and_then(|()| spawn(&bytes)) {
        Ok(c) => c,
        Err(e) => return failed(e),
    };
    if let Err(e) = prepare(&spec, &mut child) {
        let _ = child.kill();
        let _ = child.wait();
        return failed(e);
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    Ok(if status.success() { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Starts the worker with `spec`, its length first.
fn spawn(spec: &[u8]) -> Result<Child, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .arg(WORKER)
        .env_clear()
        .envs(std::env::var_os("RUST_LOG").map(|v| ("RUST_LOG", v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("the worker: {e}"))?;
    let stdin = child.stdin.as_mut().ok_or("the worker's input")?;
    let len = u64::try_from(spec.len()).unwrap_or(u64::MAX);
    stdin
        .write_all(&len.to_le_bytes())
        .and_then(|()| stdin.write_all(spec))
        .map_err(|e| e.to_string())?;
    Ok(child)
}

/// Once the worker has its namespaces: its ids and root built, and the worker told.
/// The root `spec` asks for, as the confinement builds it.
fn root_of(spec: &pb::InstallSpec) -> korpis_confine::root::Spec {
    let volumes = spec.volumes.iter().map(|v| korpis_confine::root::Volume {
        path: v.path.clone(),
        directory: v.directory.clone(),
    });
    korpis_confine::root::Spec {
        root: spec.root.clone(),
        id_base: spec.id_base,
        volumes: volumes.collect(),
        content: spec.content.clone(),
    }
}

fn prepare(spec: &pb::InstallSpec, child: &mut Child) -> Result<(), String> {
    awaited(child.stdout.as_mut().ok_or("the worker's output")?)?;
    korpis_confine::root::build(&root_of(spec), child.id())?;
    signal(child.stdin.as_mut().ok_or("the worker's input")?).map_err(|e| e.to_string())
}

/// The worker.
fn worker_main() -> Result<ExitCode, String> {
    // It dies with the first process, which is the one the helper watches and cancels.
    rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL))
        .map_err(|e| e.to_string())?;
    let mut stdin = std::io::stdin().lock();
    let mut len = [0u8; 8];
    stdin.read_exact(&mut len).map_err(|e| e.to_string())?;
    let len = u64::from_le_bytes(len).min(SPEC_LIMIT);
    let mut bytes = Vec::new();
    (&mut stdin).take(len).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    let spec = pb::InstallSpec::decode_from_slice(&bytes).map_err(|e| format!("the spec: {e}"))?;
    let out = out_of(&spec)?;
    // Groups are kept across a user namespace whether it maps them or not: dropped first.
    rustix::thread::set_thread_groups(&[]).map_err(|e| format!("groups: {e}"))?;
    let flags =
        UnshareFlags::NEWUSER | UnshareFlags::NEWNS | UnshareFlags::NEWNET | UnshareFlags::NEWIPC;
    unshare(flags)?;
    signal(&mut std::io::stdout().lock()).map_err(|e| e.to_string())?;
    awaited(&mut stdin)?;
    let result = confine::outcome(&spec, &out);
    tracing::info!(job = %spec.id, ok = result.ok, done = result.done, "install ended");
    work::record(&out, "result", &result.encode_to_vec())
        .map_err(|e| format!("the result: {e}"))?;
    Ok(ExitCode::SUCCESS)
}
