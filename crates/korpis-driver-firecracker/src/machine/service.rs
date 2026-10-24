// SPDX-License-Identifier: AGPL-3.0-or-later

//! Service guests (§4.4 of docs/04-runtimes.md): guests of Korpis's own, booted from the boot
//! drive with no image, holding a stopped workload's volumes as its own guest would and running no
//! entry. One serves a stopped object's file sessions with the worker a running guest runs, and is
//! stopped, its volumes synced, when the last ends; one booted for an install also runs the
//! install job ([`super::install`]) and stays while it runs. There is one per workload at a time,
//! since a block device is attached to one guest at a time, so it is keyed by workload, not by
//! object: a first install runs before the workload has an object at all. One serving a backup's
//! staged image ([`super::image`]) holds a copy and not the workload's volume, so it is keyed by
//! the image and runs beside the workload's own guest. The object's start ends
//! a service guest serving files first, which ends its sessions, so a file manager left open does
//! not hold a workload from starting, and is refused while an install runs. The services' lock is
//! taken before the verbs' lock, never after.

use super::stop::ended;
use super::{JAILER_BOUND, Machine, MachineError, MachineResult, failed, now_ms};
use crate::jail::{self, Drives};
use crate::object::{Object, Vm};
use crate::process;
use korpis_guest::config::{Config, Role, Volume};
use korpis_proto::korpis::driver::v1 as driver;
use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What a service guest's jail is named by: this, then a digest of what it is held by, its
/// workload's name or a staged image's, which no object's identifier begins with.
const PREFIX: &str = "svc-";
/// Its config drive, in its directory.
const CONFIG: &str = "config";
/// What its jailer and Firecracker say of themselves.
const LOG: &str = "guest.log";

/// The service guests running, by workload or staged image, and the installs known, by job.
#[derive(Debug, Default)]
pub struct Services(pub(super) tokio::sync::Mutex<Running>);

#[derive(Debug, Default)]
pub(super) struct Running {
    /// The last service guest booted's number: a lease gives up only the one it holds.
    booted: u64,
    pub(super) held: HashMap<String, Held>,
    pub(super) installs: HashMap<String, super::install::Outcome>,
}

#[derive(Debug)]
pub(super) struct Held {
    pub(super) number: u64,
    pub(super) sessions: usize,
    pub(super) vm: Vm,
    /// The install it runs, until that has ended.
    pub(super) install: Option<String>,
}

impl Running {
    /// Holds `vm`, the service guest of `key`, a workload or a staged image, just booted, for
    /// `sessions` and `install`; its number.
    pub(super) fn hold(
        &mut self,
        key: &str,
        vm: Vm,
        sessions: usize,
        install: Option<String>,
    ) -> u64 {
        self.booted += 1;
        let number = self.booted;
        self.held.insert(key.to_owned(), Held { number, sessions, vm, install });
        number
    }
}

/// A file session's hold on the service guest serving it, given up when it is dropped.
#[derive(Debug)]
pub struct Lease {
    machine: Machine,
    key: String,
    number: u64,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let (machine, number) = (self.machine.clone(), self.number);
        let key = std::mem::take(&mut self.key);
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { machine.release(&key, number).await });
        }
    }
}

/// Where an install's content drive is kept while its service guest `sid` runs.
pub(super) fn content_of(settings: &crate::settings::Settings, sid: &str) -> PathBuf {
    settings.services().join(format!("{sid}.content"))
}

/// The jail of the service guest held by `key`, a workload or a staged image.
pub(super) fn service_id(key: &str) -> String {
    let digest = korpis_core::content::digest_of(key.as_bytes());
    let hex = digest.trim_start_matches("sha256:");
    format!("{PREFIX}{}", hex.get(..32).unwrap_or(hex))
}

/// What a service guest is booted for: what it is held by, the workload's spec, its role, an
/// install's content drive, and whether its one volume is a staged copy rather than the
/// workload's own.
pub(super) struct Wanted<'a> {
    pub(super) key: &'a str,
    pub(super) spec: &'a driver::PreparedSpec,
    pub(super) role: Role,
    pub(super) content: Option<PathBuf>,
    pub(super) copy: bool,
}

impl<'a> Wanted<'a> {
    /// A service guest of `spec`'s workload's own volumes, held by the workload.
    pub(super) fn own(
        spec: &'a driver::PreparedSpec,
        role: Role,
        content: Option<PathBuf>,
    ) -> Self {
        Self { key: &spec.workload, spec, role, content, copy: false }
    }
}

impl Machine {
    /// A hold on the service guest of stopped object `id`'s workload, booted if none runs, and
    /// its vsock socket.
    pub(super) async fn service(&self, id: &str) -> MachineResult<(PathBuf, Lease)> {
        let object = self.load(id)?;
        let workload = object.spec.workload.clone();
        let mut running = self.services.0.lock().await;
        let socket = jail::vsock_socket(&self.settings, &service_id(&workload));
        let number = if let Some(held) = running.held.get_mut(&workload) {
            held.sessions += 1;
            held.number
        } else {
            let vm = self.boot(&Wanted::own(&object.spec, Role::Files, None)).await?;
            running.hold(&workload, vm, 1, None)
        };
        Ok((socket, Lease { machine: self.clone(), key: workload, number }))
    }

    /// Gives up one hold on service guest `number` of `key`, stopped with the last unless it
    /// still runs an install.
    async fn release(&self, key: &str, number: u64) {
        let mut running = self.services.0.lock().await;
        let Some(held) = running.held.get_mut(key).filter(|h| h.number == number) else {
            return;
        };
        held.sessions = held.sessions.saturating_sub(1);
        if held.sessions == 0
            && held.install.is_none()
            && let Some(held) = running.held.remove(key)
        {
            self.halt(key, &held.vm).await;
        }
    }

    /// A lease on service guest `number` of `key`, for a session that has taken it.
    pub(super) fn lease(&self, key: &str, number: u64) -> Lease {
        Lease { machine: self.clone(), key: key.to_owned(), number }
    }

    /// Ends `workload`'s service guest, if one serves files, before its own guest takes its
    /// volumes, and refuses while one runs an install unless `even_an_install`, as a destroy
    /// ends one: the services' lock, held until the caller's verb is done.
    pub(super) async fn preempt(
        &self,
        workload: &str,
        even_an_install: bool,
    ) -> MachineResult<tokio::sync::MutexGuard<'_, impl Sized>> {
        let mut running = self.services.0.lock().await;
        if !even_an_install && running.held.get(workload).is_some_and(|h| h.install.is_some()) {
            let why = format!("an install is writing {workload}'s volumes");
            return Err(MachineError::Refused(why));
        }
        if let Some(held) = running.held.remove(workload) {
            tracing::info!(%workload, sessions = held.sessions, "a start ends its service guest");
            self.halt(workload, &held.vm).await;
        }
        Ok(running)
    }

    /// Boots the service guest `wanted` asks for, once no object of its workload runs, unless
    /// what it holds is a copy.
    pub(super) async fn boot(&self, wanted: &Wanted<'_>) -> MachineResult<Vm> {
        let _held = self.lock.lock().await;
        let spec = wanted.spec;
        let runs = |o: &Object| {
            o.spec.workload == spec.workload && o.vm.is_some_and(|v| process::alive(&v))
        };
        if !wanted.copy && Object::all(&self.settings.objects()).iter().any(runs) {
            let why = format!("{} runs: its own guest holds its volumes", spec.workload);
            return Err(MachineError::Refused(why));
        }
        let sid = service_id(wanted.key);
        let dir = self.settings.services().join(&sid);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::DirBuilder::new().mode(0o700).create(&dir).map_err(failed)?;
        let config = configured(&dir, wanted)?;
        let drives = self.drives(wanted, config).await?;
        let mut shape = crate::guest::shape(spec);
        shape.netns = None;
        jail::remove(&self.settings, &sid).map_err(failed)?;
        jail::build(&self.settings, &sid, &drives, &shape).map_err(failed)?;
        let owner = (spec.uid_base, spec.uid_base);
        let mut command = jail::command(&self.settings, &sid, owner, &shape);
        let log = std::fs::File::create(dir.join(LOG)).map_err(failed)?;
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(log);
        let vm = match self.jailed(&sid, command, owner.0).await {
            Ok(vm) => vm,
            Err(e) => {
                let _ = std::fs::write(self.settings.cgroup_of(&sid).join("cgroup.kill"), "1");
                let _ = jail::remove(&self.settings, &sid);
                return Err(e);
            }
        };
        started(&dir.join(CONFIG)).await;
        Ok(vm)
    }

    /// The drives of the service guest `wanted` asks for: the node's kernel and boot drive, the
    /// boot drive again or the content drive in the image's place, its config drive, and the
    /// workload's volumes, each given to its guest's own id; a copy is attached writable, so its
    /// journal can be replayed into it, and mounted as the spec says.
    async fn drives(&self, wanted: &Wanted<'_>, config: PathBuf) -> MachineResult<Drives> {
        let mut volumes = Vec::new();
        for m in &wanted.spec.mounts {
            let source = PathBuf::from(&m.source);
            crate::volume::give(&source, wanted.spec.uid_base).map_err(failed)?;
            volumes.push((source, m.read_only && !wanted.copy));
        }
        let unset = Drives {
            kernel: PathBuf::new(),
            boot: PathBuf::new(),
            image: PathBuf::new(),
            config,
            volumes,
        };
        let settings = Arc::clone(&self.settings);
        let current = tokio::task::spawn_blocking(move || super::create::current(&settings, unset));
        let drives = current.await.map_err(failed)?.map_err(failed)?;
        let image = wanted.content.clone().unwrap_or_else(|| drives.boot.clone());
        Ok(Drives { image, ..drives })
    }

    /// Has the jailer start `command`, jail `sid`'s, and the Firecracker it names, running as
    /// `uid`.
    async fn jailed(
        &self,
        sid: &str,
        command: std::process::Command,
        uid: u32,
    ) -> MachineResult<Vm> {
        let mut jailer = tokio::process::Command::from(command).spawn().map_err(failed)?;
        match tokio::time::timeout(JAILER_BOUND, jailer.wait()).await {
            Ok(Ok(s)) if s.success() => {}
            _ => return Err(failed("the jailer did not start the service guest")),
        }
        let pid = self.vmm_pid(sid, uid).await?;
        let start_ticks = process::start_ticks(pid).unwrap_or(0);
        Ok(Vm { pid, start_ticks, started_at_ms: now_ms(), ended_at_ms: None })
    }

    /// Stops `key`'s service guest `vm` as a guest is stopped, so its volumes are synced, then
    /// removes its jail and its directory.
    pub(super) async fn halt(&self, key: &str, vm: &Vm) {
        let sid = service_id(key);
        ended(&jail::api_socket(&self.settings, &sid), vm).await;
        if let Err(e) = jail::remove(&self.settings, &sid) {
            tracing::warn!(%key, error = %e, "a service guest's jail could not be removed");
        }
        let _ = std::fs::remove_dir_all(self.settings.services().join(&sid));
        let _ = std::fs::remove_file(content_of(&self.settings, &sid));
    }

    /// Ends every service guest a driver before this one left, their cgroups' processes killed
    /// and their jails removed: what they served ended with that driver, and an install it ran
    /// is asked again, as one whose worker was lost.
    pub(super) fn sweep(&self) {
        let jails = std::fs::read_dir(self.settings.jails_of_firecracker());
        for entry in jails.into_iter().flatten().flatten() {
            let sid = entry.file_name().to_string_lossy().into_owned();
            if !sid.starts_with(PREFIX) {
                continue;
            }
            let _ = std::fs::write(self.settings.cgroup_of(&sid).join("cgroup.kill"), "1");
            if let Err(e) = jail::remove(&self.settings, &sid) {
                tracing::warn!(jail = %sid, error = %e, "a service guest left behind stays");
            }
            let _ = std::fs::remove_dir_all(self.settings.services().join(&sid));
            let _ = std::fs::remove_file(content_of(&self.settings, &sid));
        }
    }
}

/// Writes the config drive of the service guest `wanted` asks for, in its directory `dir`:
/// its volumes at their paths and nothing to run, no network, identity, or terminal, owned by its
/// Firecracker. Made afresh, it says nothing has started.
fn configured(dir: &Path, wanted: &Wanted<'_>) -> MachineResult<PathBuf> {
    let spec = wanted.spec;
    let volumes = spec
        .mounts
        .iter()
        .map(|m| Volume { path: m.destination.clone(), read_only: m.read_only })
        .collect();
    let guest = Config {
        hostname: spec.workload.clone(),
        argv: Vec::new(),
        env: BTreeMap::new(),
        working_dir: "/".into(),
        uid: 0,
        gid: 0,
        network: None,
        volumes,
        interactive: false,
        identity: false,
        role: wanted.role,
    };
    let config = dir.join(CONFIG);
    guest.write(&mut std::fs::File::create(&config).map_err(failed)?).map_err(failed)?;
    let owner = Some(spec.uid_base);
    std::os::unix::fs::chown(&config, owner, owner).map_err(failed)?;
    Ok(config)
}

/// Waits until the service guest's config drive at `config` says it has started, within a
/// bound: its agent and files port listen from then.
async fn started(config: &Path) {
    let until = Instant::now() + JAILER_BOUND;
    let has = || {
        let drive = std::fs::File::open(config);
        drive.and_then(|mut d| korpis_guest::status::has_started(&mut d)).unwrap_or(false)
    };
    while !has() && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
