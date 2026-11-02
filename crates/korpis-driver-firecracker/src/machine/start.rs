// SPDX-License-Identifier: AGPL-3.0-or-later

//! `Start`: the jailer starts the guest's Firecracker, detached, as the workload's own id, in the
//! guest's cgroup, with the node's drives as they are now, and it boots; or, resuming a checkpoint,
//! with the drives the guest booted from, configured with nothing, and it loads the state.

use super::{JAILER_BOUND, Machine, MachineError, MachineResult, START_LOG, create, failed};
use super::{now_ms, read_pid};
use crate::jail::{self, Drives, Shape};
use crate::object::{Object, Vm};
use crate::{console, process};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

impl Machine {
    /// Starts object `id`'s guest; one already running is left so. A guest serving its file
    /// sessions while it was stopped is ended first.
    pub async fn start(&self, id: &str) -> MachineResult<()> {
        self.run(id, None).await
    }

    /// Starts object `id`'s guest as [`Self::start`] does, resuming the state in `resume`, a
    /// checkpoint's directory, if any, instead of booting it.
    pub(super) async fn run(&self, id: &str, resume: Option<OwnedFd>) -> MachineResult<()> {
        let workload = self.load(id)?.spec.workload;
        let _services = self.preempt(&workload, false).await?;
        let _held = self.lock.lock().await;
        let mut object = self.load(id)?;
        if object.vm.as_ref().is_some_and(process::alive) {
            return Ok(());
        }
        let cores = self.settings.cores();
        let (shape, cpuset) =
            crate::cores::shaped(&cores, &object.spec).map_err(MachineError::Refused)?;
        let dir = self.dir(id)?;
        let resuming = resume.is_some();
        let drives = self.drives_of(&dir, resume, object.spec.uid_base).await?;
        Object::keep_drives(&dir, &drives).map_err(failed)?;
        jail::remove(&self.settings, id).map_err(failed)?;
        jail::build(&self.settings, id, &drives, &shape).map_err(failed)?;
        if resuming {
            super::checkpoint::place(&self.settings, id, &dir, object.spec.uid_base)
                .map_err(failed)?;
        }
        self.relay(id, &object.spec)?;
        let pid = self.launch(id, &object, &shape, resuming).await?;
        if let Some(set) = &cpuset {
            let dir = self.settings.cgroup_of(id);
            if let Err(why) = crate::cores::settle(&cores, &dir, pid, set).await {
                let _ = std::fs::write(dir.join("cgroup.kill"), "1");
                return Err(failed(why));
            }
        }
        let start_ticks = process::start_ticks(pid).unwrap_or(0);
        let vm = Vm { pid, start_ticks, started_at_ms: now_ms(), ended_at_ms: None };
        if resuming && let Err(e) = super::checkpoint::load(&self.settings, id, &dir).await {
            process::kill(&vm);
            return Err(e);
        }
        object.record(&dir, vm).map_err(failed)?;
        self.watch(id, vm);
        self.changed(id);
        Ok(())
    }

    /// The drives object `id`, in `dir`, starts with: the node's kernel and boot drive as they are
    /// now, or, resuming the state in `resume`, the drives that state was taken holding, its
    /// config drive the object's own from then on.
    async fn drives_of(
        &self,
        dir: &Path,
        resume: Option<OwnedFd>,
        uid: u32,
    ) -> MachineResult<Drives> {
        let kept = Object::drives(dir).map_err(failed)?;
        let settings = Arc::clone(&self.settings);
        let dir = dir.to_owned();
        let made = tokio::task::spawn_blocking(move || match resume {
            None => create::current(&settings, kept),
            Some(from) => super::checkpoint::resumed(&settings, &dir, &from, uid, kept),
        });
        made.await.map_err(failed)?.map_err(failed)
    }

    /// Has the jailer start object `id`'s Firecracker, shaped `shape`, its console attached, and
    /// configured to boot unless `resuming`; its process id.
    async fn launch(
        &self,
        id: &str,
        object: &Object,
        shape: &Shape,
        resuming: bool,
    ) -> MachineResult<i32> {
        let dir = self.dir(id)?;
        let owner = (object.spec.uid_base, object.spec.uid_base);
        let mut command = jail::command(&self.settings, id, owner, shape, !resuming);
        // What the jailer and the guest's Firecracker say of themselves is kept beside the
        // object; the guest's console is their standard output. A resumed guest has started.
        Object::untell(&dir).map_err(failed)?;
        if !resuming {
            let mut drive = std::fs::OpenOptions::new().write(true).open(Object::config(&dir));
            drive.as_mut().map(korpis_guest::status::clear).map_err(failed)?.map_err(failed)?;
        }
        let log = std::fs::File::create(dir.join(START_LOG)).map_err(failed)?;
        let stream = object.spec.stream.as_option();
        let console = console::attach(&self.settings.writer, &dir, stream).await.map_err(failed)?;
        command.stdin(console.stdin).stdout(console.stdout).stderr(log);
        let mut jailer = tokio::process::Command::from(command).spawn().map_err(failed)?;
        let done = tokio::time::timeout(JAILER_BOUND, jailer.wait()).await;
        let said = || std::fs::read_to_string(dir.join(START_LOG));
        match done {
            Ok(Ok(s)) if s.success() => {}
            Ok(Ok(s)) => return Err(failed(format!("the jailer failed ({s}): {:?}", said()))),
            Ok(Err(e)) => return Err(failed(e)),
            Err(_) => return Err(failed("the jailer did not detach in time")),
        }
        let pid = self.vmm_pid(id, owner.0).await?;
        if let Some(writer) = console.writer {
            console::join(&writer, &self.settings.cgroup_of(id)).map_err(failed)?;
        }
        Ok(pid)
    }

    /// The process id of object `id`'s Firecracker, running as `uid`, from the pid file its
    /// jailer leaves once it has started it. The file is in the chroot, which that Firecracker
    /// can write, so what it says is believed only of a process running as `uid` in the guest's
    /// cgroup, which only root can move a process into: the driver kills what it records. The
    /// jailer names its child as soon as it is cloned, while the child is still root and has
    /// yet to drop to `uid` as it runs Firecracker, so a process named but not yet that is
    /// waited for, within the same bound.
    pub(super) async fn vmm_pid(&self, id: &str, uid: u32) -> MachineResult<i32> {
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

    /// Waits until object `id`'s run has said it started, within a bound: its serial port is open
    /// and its agent listens from then.
    pub(super) async fn until_started(&self, id: &str) -> MachineResult<()> {
        let dir = self.dir(id)?;
        let until = Instant::now() + JAILER_BOUND;
        let started = || {
            let drive = std::fs::File::open(Object::config(&dir));
            drive.and_then(|mut d| korpis_guest::status::has_started(&mut d)).unwrap_or(false)
        };
        while !started() && Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }
}
