// SPDX-License-Identifier: AGPL-3.0-or-later

//! The lifecycle of §4.2 of docs/04-runtimes.md, for guests. `Create` makes the drives and the
//! jail and runs nothing; `Start` has the jailer start Firecracker, detached, as the workload's
//! own id; a stop is Ctrl+Alt+Del and a kill a second one, then the process after a bound (§6
//! there); `Destroy` removes the jail, the cgroup, and the object. Each object is recorded on disk
//! ([`crate::object`]), so `Recover` finds every guest after the driver restarts, and each running
//! Firecracker is watched through a process descriptor, which is what `Events` reports.

mod create;
mod status;

use crate::object::{Object, Vm};
use crate::settings::Settings;
use crate::{api, jail, process};
use korpis_proto::korpis::driver::v1 as driver;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::broadcast;

/// How long a kill waits for the guest to end once told, before ending its Firecracker.
pub const KILL_BOUND: Duration = Duration::from_secs(5);

/// What the jailer said as it started the guest, in the object's directory.
pub const START_LOG: &str = "start.log";

/// The process id in the pid file at `file`, read without following a link.
fn read_pid(file: &std::path::Path) -> Option<i32> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits().cast_signed())
        .open(file)
        .ok()?;
    std::io::read_to_string(&mut f).ok()?.trim().parse().ok()
}

/// How long a kill waits between one Ctrl+Alt+Del and the next.
const AGAIN: Duration = Duration::from_millis(250);

/// How long the jailer is given to start a guest's Firecracker and detach.
const JAILER_BOUND: Duration = Duration::from_secs(10);

/// How many events a slow listener may fall behind by before it misses some.
const EVENT_BACKLOG: usize = 1024;

/// Why a verb failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineError {
    /// The spec or request is outside what this driver runs.
    #[error("refused: {0}")]
    Refused(String),
    /// No object has this identifier.
    #[error("no runtime object {0}")]
    NotFound(String),
    /// The driver tried and failed.
    #[error("{0}")]
    Failed(String),
}

/// A result of a verb.
pub type MachineResult<T> = Result<T, MachineError>;

fn failed(e: impl std::fmt::Display) -> MachineError {
    MachineError::Failed(e.to_string())
}

fn now_ms() -> u64 {
    let since = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

/// The driver's guests.
#[derive(Debug, Clone)]
pub struct Machine {
    settings: Arc<Settings>,
    events: broadcast::Sender<driver::EventsResponse>,
    /// The objects whose Firecracker is watched.
    watched: Arc<Mutex<HashSet<String>>>,
    /// One verb that changes an object at a time.
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl Machine {
    /// The guests kept in `settings.state`, its directories and its cgroup made if missing.
    pub fn new(settings: Settings) -> std::io::Result<Self> {
        for dir in [settings.objects(), settings.drives(), settings.jails()] {
            std::fs::create_dir_all(dir)?;
        }
        let parent = settings.cgroup_parent();
        std::fs::create_dir_all(&parent)?;
        for controller in ["+cpu", "+memory", "+pids", "+io"] {
            // A controller the host has not enabled is a limit not held, which describe says.
            let _ = std::fs::write(parent.join("cgroup.subtree_control"), controller);
        }
        let (events, _) = broadcast::channel(EVENT_BACKLOG);
        let lock = Arc::default();
        Ok(Self { settings: Arc::new(settings), events, watched: Arc::default(), lock })
    }

    /// Its settings.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// What changes, as it changes.
    #[must_use]
    pub fn events(&self) -> broadcast::Receiver<driver::EventsResponse> {
        self.events.subscribe()
    }

    fn dir(&self, id: &str) -> MachineResult<PathBuf> {
        let plain = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        let dir = self.settings.object(id);
        if !plain || !dir.is_dir() {
            return Err(MachineError::NotFound(id.to_owned()));
        }
        Ok(dir)
    }

    fn load(&self, id: &str) -> MachineResult<Object> {
        Object::load(&self.dir(id)?).map_err(failed)
    }

    /// Says that `id` changed.
    fn changed(&self, id: &str) {
        let status = self.inspect(id).ok();
        let event = driver::EventsResponse {
            runtime_id: id.to_owned(),
            status: status.into(),
            ..Default::default()
        };
        let _ = self.events.send(event);
    }

    /// Watches `vm`, object `id`'s Firecracker, until it ends, then records and says so.
    fn watch(&self, id: &str, vm: Vm) {
        let fresh = self.watched.lock().is_ok_and(|mut w| w.insert(id.to_owned()));
        let Some(fd) = fresh.then(|| process::descriptor(&vm)).flatten() else { return };
        let (machine, id) = (self.clone(), id.to_owned());
        tokio::spawn(async move {
            process::ended(fd).await;
            if let Ok(dir) = machine.dir(&id)
                && let Ok(mut object) = Object::load(&dir)
            {
                let ended = Vm { ended_at_ms: Some(now_ms()), ..vm };
                let _ = object.record(&dir, ended);
            }
            if let Ok(mut w) = machine.watched.lock() {
                w.remove(&id);
            }
            machine.changed(&id);
        });
    }

    /// Starts object `id`'s guest; one already running is left so.
    pub async fn start(&self, id: &str) -> MachineResult<()> {
        let _held = self.lock.lock().await;
        let mut object = self.load(id)?;
        if object.vm.as_ref().is_some_and(process::alive) {
            return Ok(());
        }
        let shape = crate::guest::shape(&object.spec);
        let dir = self.dir(id)?;
        let drives = Object::drives(&dir).map_err(failed)?;
        jail::remove(&self.settings, id).map_err(failed)?;
        jail::build(&self.settings, id, &drives, &shape).map_err(failed)?;
        let owner = (object.spec.uid_base, object.spec.uid_base);
        let mut command = jail::command(&self.settings, id, owner, &shape);
        // What the jailer says before it detaches is kept beside the object: once detached, its
        // output and Firecracker's go nowhere.
        Object::untell(&self.dir(id)?).map_err(failed)?;
        let log = std::fs::File::create(self.dir(id)?.join(START_LOG)).map_err(failed)?;
        command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(log);
        let mut jailer = tokio::process::Command::from(command).spawn().map_err(failed)?;
        let done = tokio::time::timeout(JAILER_BOUND, jailer.wait()).await;
        let said = || std::fs::read_to_string(self.settings.object(id).join(START_LOG));
        match done {
            Ok(Ok(s)) if s.success() => {}
            Ok(Ok(s)) => return Err(failed(format!("the jailer failed ({s}): {:?}", said()))),
            Ok(Err(e)) => return Err(failed(e)),
            Err(_) => return Err(failed("the jailer did not detach in time")),
        }
        let pid = self.vmm_pid(id, owner.0).await?;
        let start_ticks = process::start_ticks(pid).unwrap_or(0);
        let vm = Vm { pid, start_ticks, started_at_ms: now_ms(), ended_at_ms: None };
        object.record(&self.dir(id)?, vm).map_err(failed)?;
        self.watch(id, vm);
        self.changed(id);
        Ok(())
    }

    /// The process id of object `id`'s Firecracker, running as `uid`, from the pid file its
    /// jailer leaves once it has started it. The file is in the chroot, which that Firecracker
    /// can write, so what it says is believed only of a process running as `uid` in the guest's
    /// cgroup, which only root can move a process into: the driver kills what it records.
    async fn vmm_pid(&self, id: &str, uid: u32) -> MachineResult<i32> {
        let file = jail::pid_file(&self.settings, id);
        let until = Instant::now() + JAILER_BOUND;
        let pid = loop {
            if let Some(pid) = read_pid(&file) {
                break pid;
            }
            if Instant::now() >= until {
                return Err(failed("the jailer named no Firecracker process"));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        if process::in_cgroup(pid, &self.settings.cgroup_of(id)) && process::runs_as(pid, uid) {
            return Ok(pid);
        }
        Err(failed("the jailer's pid file names no process of this guest"))
    }

    /// Tells object `id`'s guest to stop, as the driver's graceful default, and kills it once
    /// `grace` has passed if it has not ended.
    pub async fn stop(&self, id: &str, signal: &str, grace: Duration) -> MachineResult<()> {
        self.signal(id, signal).await?;
        let machine = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let _ = machine.kill(&id).await;
        });
        Ok(())
    }

    /// Sends object `id`'s guest Ctrl+Alt+Del, the one signal it can be sent (§4.2 of
    /// docs/04-runtimes.md), once in a run; a guest not running has nothing to stop.
    pub async fn signal(&self, id: &str, signal: &str) -> MachineResult<()> {
        if !signal.is_empty() && signal != "SIGTERM" {
            let why = format!("{signal}: a guest is sent no named signals, only a stop");
            return Err(MachineError::Refused(why));
        }
        let object = self.load(id)?;
        let dir = self.dir(id)?;
        if !object.vm.as_ref().is_some_and(process::alive) || !Object::tell(&dir).map_err(failed)? {
            return Ok(());
        }
        let told = api::ctrl_alt_del(&jail::api_socket(&self.settings, id)).await;
        if told.is_err() {
            let _ = Object::untell(&dir);
        }
        told.map_err(failed)
    }

    /// Kills object `id`'s guest: a second Ctrl+Alt+Del, which ends every process and has the
    /// guest sync and restart, then its Firecracker if it has not ended within [`KILL_BOUND`].
    pub async fn kill(&self, id: &str) -> MachineResult<()> {
        let object = self.load(id)?;
        let Some(vm) = object.vm.filter(process::alive) else { return Ok(()) };
        // Sent until the guest has ended: a kill with no stop before it is the guest's first, and
        // a Ctrl+Alt+Del that arrives while the guest has not yet read the last one is absorbed by
        // it, the kernel keeping one pending signal of a kind.
        let socket = jail::api_socket(&self.settings, id);
        let until = Instant::now() + KILL_BOUND;
        while process::alive(&vm) && Instant::now() < until {
            let _ = api::ctrl_alt_del(&socket).await;
            tokio::time::sleep(AGAIN).await;
        }
        process::kill(&vm);
        let until = Instant::now() + KILL_BOUND;
        while process::alive(&vm) && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }

    /// Removes object `id`: its guest killed, its jail, cgroup, and record removed.
    pub async fn destroy(&self, id: &str) -> MachineResult<()> {
        let dir = match self.dir(id) {
            Ok(d) => d,
            Err(MachineError::NotFound(_)) => return Ok(()),
            Err(e) => return Err(e),
        };
        self.kill(id).await?;
        let _held = self.lock.lock().await;
        jail::remove(&self.settings, id).map_err(failed)?;
        std::fs::remove_dir_all(dir).map_err(failed)?;
        self.changed(id);
        Ok(())
    }

    /// Every object, each running one watched again: after a restart of the driver, this is how
    /// its guests are found (§4.4 of docs/02-architecture.md).
    #[must_use]
    pub fn recover(&self) -> Vec<driver::RuntimeObject> {
        let objects = Object::all(&self.settings.objects());
        for o in &objects {
            if let Some(vm) = o.vm.filter(process::alive) {
                self.watch(&o.id, vm);
            }
        }
        objects
            .into_iter()
            .map(|o| driver::RuntimeObject {
                runtime_id: o.id.clone(),
                labels: o.spec.labels.clone(),
                status: self.inspect(&o.id).ok().into(),
                ..Default::default()
            })
            .collect()
    }
}
