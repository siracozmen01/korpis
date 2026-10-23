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
use crate::{api, console, jail, process};
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

/// The process id in the pid file at `file`. The guest's Firecracker can write where it is, so
/// it is opened without following a link and without waiting, believed only if it is a regular
/// file, and read no further than a process id can run: a FIFO or an endless file put there
/// stalls nothing.
fn read_pid(file: &std::path::Path) -> Option<i32> {
    use rustix::fs::{FileType, Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix::fs::open(file, flags, Mode::empty()).ok()?;
    let stat = rustix::fs::fstat(&fd).ok()?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return None;
    }
    let mut text = [0u8; 32];
    let n = rustix::io::read(&fd, &mut text).ok()?;
    std::str::from_utf8(text.get(..n)?).ok()?.trim().parse().ok()
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
        let kept = Object::drives(&dir).map_err(failed)?;
        let settings = Arc::clone(&self.settings);
        let current = tokio::task::spawn_blocking(move || create::current(&settings, kept));
        let drives = current.await.map_err(failed)?.map_err(failed)?;
        Object::keep_drives(&dir, &drives).map_err(failed)?;
        jail::remove(&self.settings, id).map_err(failed)?;
        jail::build(&self.settings, id, &drives, &shape).map_err(failed)?;
        let owner = (object.spec.uid_base, object.spec.uid_base);
        let mut command = jail::command(&self.settings, id, owner, &shape);
        // What the jailer and the guest's Firecracker say of themselves is kept beside the
        // object; the guest's console is their standard output.
        Object::untell(&dir).map_err(failed)?;
        let mut drive = std::fs::OpenOptions::new().write(true).open(Object::config(&dir));
        drive.as_mut().map(korpis_guest::status::clear).map_err(failed)?.map_err(failed)?;
        let log = std::fs::File::create(dir.join(START_LOG)).map_err(failed)?;
        let stream = object.spec.stream.as_option();
        let console = console::attach(&self.settings.writer, &dir, stream).await.map_err(failed)?;
        let writer = console.writer;
        command.stdin(console.stdin).stdout(console.stdout).stderr(log);
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
        if let Some(writer) = writer {
            console::join(&writer, &self.settings.cgroup_of(id)).map_err(failed)?;
        }
        let start_ticks = process::start_ticks(pid).unwrap_or(0);
        let vm = Vm { pid, start_ticks, started_at_ms: now_ms(), ended_at_ms: None };
        object.record(&dir, vm).map_err(failed)?;
        self.watch(id, vm);
        self.changed(id);
        Ok(())
    }

    /// The process id of object `id`'s Firecracker, running as `uid`, from the pid file its
    /// jailer leaves once it has started it. The file is in the chroot, which that Firecracker
    /// can write, so what it says is believed only of a process running as `uid` in the guest's
    /// cgroup, which only root can move a process into: the driver kills what it records. The
    /// jailer names its child as soon as it is cloned, while the child is still root and has
    /// yet to drop to `uid` as it runs Firecracker, so a process named but not yet that is
    /// waited for, within the same bound.
    async fn vmm_pid(&self, id: &str, uid: u32) -> MachineResult<i32> {
        let file = jail::pid_file(&self.settings, id);
        let cgroup = self.settings.cgroup_of(id);
        let until = Instant::now() + JAILER_BOUND;
        let mut named = None;
        while Instant::now() < until {
            named = read_pid(&file).or(named);
            let ours = |pid: &i32| process::in_cgroup(*pid, &cgroup) && process::runs_as(*pid, uid);
            if let Some(pid) = named.filter(ours) {
                return Ok(pid);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err(failed(match named {
            Some(_) => "the jailer's pid file names no process of this guest",
            None => "the jailer named no Firecracker process",
        }))
    }

    /// Sends `data`, typed by `from` (who, and in which session), to object `id`'s console, once
    /// its guest has started: what reaches the serial port before the guest's kernel has opened
    /// it is lost, so an input sent as a guest boots waits for it, within a bound.
    pub async fn console_input(
        &self,
        id: &str,
        from: (&str, &str),
        data: &[u8],
    ) -> MachineResult<()> {
        let object = self.load(id)?;
        let dir = self.dir(id)?;
        let until = Instant::now() + JAILER_BOUND;
        let started = || {
            let drive = std::fs::File::open(Object::config(&dir));
            drive.and_then(|mut d| korpis_guest::status::has_started(&mut d)).unwrap_or(false)
        };
        while !started() && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        console::send(&dir, object.spec.stream.as_option(), from, data).map_err(|(refused, why)| {
            if refused { MachineError::Refused(why) } else { MachineError::Failed(why) }
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pid_file_is_believed_only_as_a_short_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("firecracker.pid");
        std::fs::write(&file, "4242\n").unwrap();
        assert_eq!(read_pid(&file), Some(4242));
        let link = dir.path().join("link.pid");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(read_pid(&link), None, "a link is not followed");
        let fifo = dir.path().join("fifo.pid");
        korpis_driver_kit::console::make_fifo(&fifo).unwrap();
        assert_eq!(read_pid(&fifo), None, "a FIFO is refused, not waited on");
        let long = dir.path().join("long.pid");
        std::fs::write(&long, format!("4242{}", " ".repeat(1 << 20))).unwrap();
        assert_eq!(read_pid(&long), Some(4242), "read no further than a pid runs");
    }
}
