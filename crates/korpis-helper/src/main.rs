// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-helper`: the node's root helper. Its arguments are parsed by hand, so the one root
//! process carries no argument-parsing library.

use korpis_helper::helper::Helper;
use korpis_helper::ids::Ledger;
use korpis_helper::job::JobConfig;
use korpis_helper::serve::{bound, inherited, serve};
use korpis_helper::storage::Class;
use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage: korpis-helper (--inherit | --socket PATH) [--volume-root PATH] \
                     [--class NAME] [--state PATH] [--allow-uid UID]... [--idle SECONDS] \
                     [--job-binary PATH] [--job-cgroups PATH|none] [--content PATH]";

/// What the helper was told.
struct Args {
    /// The socket to bind; none when systemd passes it as standard input.
    socket: Option<PathBuf>,
    volume_root: PathBuf,
    class: String,
    state: PathBuf,
    allow: Vec<u32>,
    idle: Option<Duration>,
    /// The job binary; beside this one when not given.
    job_binary: Option<PathBuf>,
    job_cgroups: Option<PathBuf>,
    content: PathBuf,
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        socket: None,
        volume_root: PathBuf::from("/var/lib/korpis/volumes"),
        class: "local".into(),
        state: PathBuf::from("/var/lib/korpis/helper"),
        allow: Vec::new(),
        idle: None,
        job_binary: None,
        job_cgroups: Some(PathBuf::from("/sys/fs/cgroup/korpis")),
        content: PathBuf::from("/var/lib/korpis-agent/content/blobs/sha256"),
    };
    let mut inherit = false;
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--inherit" => inherit = true,
            "--socket" => a.socket = Some(PathBuf::from(value()?)),
            "--volume-root" => a.volume_root = PathBuf::from(value()?),
            "--class" => a.class = value()?,
            "--state" => a.state = PathBuf::from(value()?),
            "--allow-uid" => a.allow.push(value()?.parse().map_err(|e| format!("{e}"))?),
            "--job-binary" => a.job_binary = Some(PathBuf::from(value()?)),
            "--job-cgroups" => {
                let v = value()?;
                a.job_cgroups = (v != "none").then(|| PathBuf::from(v));
            }
            "--content" => a.content = PathBuf::from(value()?),
            "--idle" => {
                a.idle = Some(Duration::from_secs(value()?.parse().map_err(|e| format!("{e}"))?));
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if inherit == a.socket.is_some() {
        return Err("exactly one of --inherit and --socket".into());
    }
    if inherit && a.idle.is_none() {
        a.idle = Some(Duration::from_secs(60));
    }
    Ok(a)
}

fn run(a: Args) -> Result<(), String> {
    let own = std::fs::metadata("/proc/self").map_err(|e| e.to_string())?.uid();
    let allowed: BTreeSet<u32> =
        if a.allow.is_empty() { [own].into() } else { a.allow.into_iter().collect() };
    let class = Class::open(&a.class, &a.volume_root).map_err(|e| format!("volume root: {e}"))?;
    let ledger = Ledger::open(&a.state).map_err(|e| format!("state: {e}"))?;
    let listener = match &a.socket {
        Some(path) => bound(path),
        None => inherited(),
    }
    .map_err(|e| format!("socket: {e}"))?;
    let binary = match a.job_binary {
        Some(b) => b,
        None => {
            std::env::current_exe().map_err(|e| e.to_string())?.with_file_name("korpis-backup-job")
        }
    };
    let jobs = JobConfig {
        binary,
        state: a.state.clone(),
        cgroups: a.job_cgroups,
        content: Some(a.content),
    };
    let mut helper = Helper::new(vec![class], ledger).with_jobs(jobs);
    tracing::info!(?allowed, "serving");
    serve(&listener, &allowed, &mut helper, a.idle).map_err(|e| e.to_string())
}

#[expect(clippy::print_stderr, reason = "a usage error is the binary's own output")]
fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1).peekable();
    if args.peek().map(String::as_str) == Some("--version") {
        eprintln!("{}", korpis_core::build::describe("korpis-helper"));
        return ExitCode::SUCCESS;
    }
    let a = match parse(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(a) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "the helper stopped");
            ExitCode::FAILURE
        }
    }
}
