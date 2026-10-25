// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-helper`: the node's root helper. Its arguments are parsed by hand, so the one root
//! process carries no argument-parsing library.

use korpis_helper::cores::{Cores, CoresConfig};
use korpis_helper::files::{Files, FilesConfig};
use korpis_helper::helper::Helper;
use korpis_helper::ids::Ledger;
use korpis_helper::ingress::{Ingress, IngressConfig};
use korpis_helper::job::JobConfig;
use korpis_helper::network::Network;
use korpis_helper::serve::{bound, inherited, serve};
use korpis_helper::storage::Class;
use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage: korpis-helper (--inherit | --socket PATH) [--volume-root PATH] \
                     [--class NAME] [--state PATH] [--allow-uid UID]... [--idle SECONDS] \
                     [--job-binary PATH] [--install-binary PATH|none] [--job-cgroups PATH|none] \
                     [--content PATH] [--netns-root PATH|none] \
                     [--ingress-binary PATH|none] [--files-binary PATH|none] \
                     [--cores-cgroup PATH|none]...\n       \
                     korpis-helper release-networks [--netns-root PATH]";

/// Where workloads' network namespaces are bound unless `--netns-root` says otherwise.
const NETNS_ROOT: &str = "/run/korpis/netns";

/// Where the install job binary is, if install jobs are run.
enum InstallBinary {
    /// Beside this binary, which is where a release puts it.
    Beside,
    /// Where `--install-binary` says.
    At(PathBuf),
    /// Nowhere: `--install-binary none` runs no install jobs.
    None,
}

impl InstallBinary {
    /// What a binary's argument says: `none`, or a path.
    fn from_arg(v: String) -> Self {
        if v == "none" { Self::None } else { Self::At(v.into()) }
    }

    /// The binary, `name` beside this one when none was given.
    fn resolve(self, name: &str) -> Result<Option<PathBuf>, String> {
        Ok(match self {
            Self::Beside => {
                Some(std::env::current_exe().map_err(|e| e.to_string())?.with_file_name(name))
            }
            Self::At(b) => Some(b),
            Self::None => None,
        })
    }
}

/// What the helper was told.
struct Args {
    /// The socket to bind; none when systemd passes it as standard input.
    socket: Option<PathBuf>,
    volume_root: PathBuf,
    class: String,
    state: PathBuf,
    allow: Vec<u32>,
    idle: Option<Duration>,
    /// The backup job binary; beside this one when not given.
    job_binary: Option<PathBuf>,
    /// The install job binary.
    install_binary: InstallBinary,
    /// The ingress proxy's binary.
    ingress_binary: InstallBinary,
    /// The file session worker's binary.
    files_binary: InstallBinary,
    job_cgroups: Option<PathBuf>,
    content: PathBuf,
    /// Where workloads' network namespaces are bound; none gives workloads no network.
    netns_root: Option<PathBuf>,
    /// The drivers' cgroup parents whose cpusets it keeps for dedicated cores; `None` until one
    /// is named, which keeps the drivers' own defaults.
    cores_cgroups: Option<Vec<PathBuf>>,
}

/// The cgroup parents the first-party drivers use unless told otherwise.
const CORES_CGROUPS: [&str; 2] = ["/korpis", "/korpis-firecracker"];

fn parse(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        socket: None,
        volume_root: PathBuf::from("/var/lib/korpis/volumes"),
        class: "local".into(),
        state: PathBuf::from("/var/lib/korpis/helper"),
        allow: Vec::new(),
        idle: None,
        job_binary: None,
        install_binary: InstallBinary::Beside,
        ingress_binary: InstallBinary::Beside,
        files_binary: InstallBinary::Beside,
        // Within the `oci` driver's shared cgroup, whose cpuset is kept off dedicated cores.
        job_cgroups: Some(PathBuf::from("/sys/fs/cgroup/korpis/shared")),
        content: PathBuf::from("/var/lib/korpis-agent/content"),
        netns_root: Some(PathBuf::from(NETNS_ROOT)),
        cores_cgroups: None,
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
            "--install-binary" => a.install_binary = InstallBinary::from_arg(value()?),
            "--ingress-binary" => a.ingress_binary = InstallBinary::from_arg(value()?),
            "--files-binary" => a.files_binary = InstallBinary::from_arg(value()?),
            "--job-cgroups" => {
                let v = value()?;
                a.job_cgroups = (v != "none").then(|| PathBuf::from(v));
            }
            "--content" => a.content = PathBuf::from(value()?),
            "--netns-root" => {
                let v = value()?;
                a.netns_root = (v != "none").then(|| PathBuf::from(v));
            }
            "--cores-cgroup" => {
                let v = value()?;
                let kept = a.cores_cgroups.get_or_insert_with(Vec::new);
                kept.extend((v != "none").then(|| PathBuf::from(v)));
            }
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
    let beside = |name: &str| -> Result<PathBuf, String> {
        Ok(std::env::current_exe().map_err(|e| e.to_string())?.with_file_name(name))
    };
    let binary = match a.job_binary {
        Some(b) => b,
        None => beside("korpis-backup-job")?,
    };
    let install_binary = a.install_binary.resolve("korpis-install-job")?;
    let ingress_binary = a.ingress_binary.resolve("korpis-ingress")?;
    let files_binary = a.files_binary.resolve("korpis-fsworker")?;
    let jobs = JobConfig {
        binary,
        install_binary,
        state: a.state.clone(),
        cgroups: a.job_cgroups.clone(),
        content: Some(a.content),
    };
    let mut helper = Helper::new(vec![class], ledger).with_jobs(jobs);
    if let Some(binary) = files_binary {
        let config = FilesConfig { binary, state: a.state.clone(), cgroups: a.job_cgroups.clone() };
        helper = helper.with_files(Files::new(config));
    }
    if let Some(binary) = ingress_binary {
        helper =
            helper.with_ingress(Ingress::new(IngressConfig { binary, cgroups: a.job_cgroups }));
    }
    if let Some(root) = a.netns_root {
        helper = helper.with_network(Network::new(root));
    }
    let parents = a.cores_cgroups.unwrap_or_else(|| CORES_CGROUPS.map(PathBuf::from).to_vec());
    if !parents.is_empty() {
        let config = CoresConfig {
            root: PathBuf::from("/sys/fs/cgroup"),
            online: PathBuf::from("/sys/devices/system/cpu/online"),
            parents,
        };
        helper = helper.with_cores(Cores::new(config).map_err(|e| format!("cores: {e}"))?);
    }
    tracing::info!(?allowed, "serving");
    serve(&listener, &allowed, &mut helper, a.idle).map_err(|e| e.to_string())
}

/// `korpis-helper release-networks`: releases every workload network bound under the netns root
/// and exits, for a machine whose node did not destroy its workloads itself (§3 of
/// docs/18-operations.md). Each network released is printed; one that could not be is said, and
/// the command fails.
#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "what was released, and what was not, is the command's own output"
)]
fn release_networks(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut root = PathBuf::from(NETNS_ROOT);
    while let Some(flag) = args.next() {
        let value = args.next();
        let (Some(v), "--netns-root") = (value, flag.as_str()) else {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        };
        root = PathBuf::from(v);
    }
    let (released, failed) = Network::new(root).release_all();
    for w in &released {
        println!("released {w}");
    }
    for (w, e) in &failed {
        eprintln!("{w}: {e}");
    }
    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
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
    if args.peek().map(String::as_str) == Some("release-networks") {
        return release_networks(args.skip(1));
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
