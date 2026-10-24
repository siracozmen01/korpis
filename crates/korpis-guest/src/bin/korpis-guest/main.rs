// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-guest`, the first process of every microVM (§4.4 and §6 of docs/04-runtimes.md). The
//! kernel mounts the boot drive as its root and starts it. It reads its config drive, assembles
//! the workload's root (the image beneath a small writable layer, made read-only once its mount
//! points exist, as a container's root is), mounts the volumes, sets `eth0`, and runs the entry
//! as its user, reaping whatever the entry's own children leave behind. A stop arrives as
//! Ctrl+Alt+Del and reaches the entry as `SIGTERM`; a second ends every process. When the entry
//! has ended, it ends every other process, writes how the run ended on the config drive, and
//! restarts the guest, which ends the microVM: Firecracker exits when its guest reboots. The first
//! process must never return, so every failure ends the same way, with the reason on the console.
//! While the entry runs, it answers the driver on the control port over vsock. A service guest
//! mounts its volumes in a root of nothing else and serves file sessions, running no entry.
//!
//! It holds two `unsafe` blocks. In `run`, the entry's child clears the signal mask it inherits
//! before exec, and an interactive entry takes its terminal as its controlling one, which only
//! `pre_exec` can do; in `grow`, a volume's filesystem is grown to its drive by an `ioctl` that
//! has no safe form.

mod control;
mod exec;
mod fileport;
mod files;
mod grow;
mod install;
mod mounts;
mod net;
mod ready;
mod relay;
mod run;

use korpis_guest::config::{Config, Role};
use korpis_guest::drive;
use korpis_guest::status::Ended;
use nix::sys::reboot::{RebootMode, reboot, set_cad_enabled};
use nix::sys::signal::{SigSet, Signal};
use nix::sys::signalfd::{SfdFlags, SignalFd};
use std::fs::{File, OpenOptions};
use std::time::Duration;

/// Where the workload's root is assembled, on the boot drive.
const ROOT: &str = "/newroot";
/// The file worker on the boot drive (§4.4 of docs/04-runtimes.md).
const FILE_WORKER: &str = "/korpis-fsworker";
/// The install job on the boot drive, which a service guest runs for an install.
const INSTALL_JOB: &str = "/korpis-install-job";
/// How long the entry waits for the workload's files once its identity socket is served: the
/// driver sends them within a fraction of a second of being able to, so this is for a driver
/// that is itself restarting.
const FIRST_SYNC: Duration = Duration::from_secs(30);

/// A step that failed, with what it was.
fn failed<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> String {
    move |e| format!("{what}: {e}")
}

/// Says `line` on the console, which is the guest's serial port and the workload's own console.
#[expect(clippy::print_stderr, reason = "the console is the guest's only output")]
fn say(line: &str) {
    eprintln!("korpis-guest: {line}");
}

/// The signals the first process takes through a descriptor rather than a handler: a stop, and
/// a child's end. Blocked here, they stay blocked for nothing else, since a spawned process
/// starts with its mask cleared.
fn signals() -> Result<SignalFd, String> {
    let mut mask = SigSet::empty();
    for s in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGCHLD] {
        mask.add(s);
    }
    mask.thread_block().map_err(failed("blocking signals"))?;
    SignalFd::with_flags(&mask, SfdFlags::SFD_NONBLOCK | SfdFlags::SFD_CLOEXEC)
        .map_err(failed("a signal descriptor"))
}

/// The console's output as the entry writes it. A terminal's output processing turns each
/// newline into a carriage return and a newline; with it off, a console of logs holds what a
/// container's would. An interactive console keeps it, as a container's terminal does.
fn plain_output() {
    use rustix::termios::{OptionalActions, OutputModes, tcgetattr, tcsetattr};
    let console = std::io::stdout();
    let set = tcgetattr(&console).and_then(|mut modes| {
        modes.output_modes.remove(OutputModes::OPOST);
        tcsetattr(&console, OptionalActions::Now, &modes)
    });
    if let Err(e) = set {
        say(&format!("the console's output modes: {e}"));
    }
}

/// Everything up to the entry's end, on the config drive `drive`.
fn boot(drive: &mut File) -> Result<Ended, String> {
    // Ctrl+Alt+Del becomes SIGINT to this process rather than an immediate restart, so a stop
    // reaches the entry first.
    set_cad_enabled(false).map_err(failed("taking Ctrl+Alt+Del"))?;
    let mut signals = signals()?;
    let config = Config::read(drive).map_err(failed("reading the config drive"))?;
    // Listening before the run says it has started, so a driver told it has can ask the agent
    // and open a session; each waits for the workload's root (`ready`). The file worker is on the
    // boot drive, which the workload's root does not hold.
    if let Err(e) = control::serve(std::time::Instant::now(), std::sync::Arc::new(config.clone())) {
        say(&format!("{e}; the guest's agent will not answer"));
    }
    let worker = File::open(FILE_WORKER).map_err(|e| format!("the file worker: {e}"));
    let serving = worker.and_then(fileport::serve);
    // The serial port was opened for this process before it ran, and what is typed on it from
    // now on is kept for whoever reads it: the driver holds a console's input until it is told.
    if let Err(e) = korpis_guest::status::started(drive) {
        say(&format!("saying the run has started: {e}"));
    }
    // The install job is on the boot drive too.
    let install_job = File::open(INSTALL_JOB).map_err(|e| format!("the install job: {e}"));
    if config.role.is_service() {
        serving?;
        return service(&config, install_job, &mut signals);
    }
    if let Err(e) = serving {
        say(&format!("{e}; the guest serves no file session"));
    }
    if !config.interactive {
        plain_output();
    }
    nix::unistd::sethostname(&config.hostname).map_err(failed("setting the hostname"))?;
    mounts::root(ROOT, &config)?;
    net::configure(config.network.as_ref())?;
    enter(ROOT).map_err(failed("entering the workload's root"))?;
    if config.identity {
        relay::serve()?;
        if !files::synced(FIRST_SYNC) {
            say("the workload's files did not arrive in time; its entry starts without them");
        }
    }
    ready::set();
    run::run(&config, &mut signals)
}

/// A service guest's run (§4.4 of docs/04-runtimes.md): its volumes in a root of nothing else,
/// file sessions served on the files port, an install run if it was booted for one, and no
/// entry, until it is stopped. Without its worker it has nothing to do, so it ends at once.
fn service(
    config: &Config,
    install_job: Result<File, String>,
    signals: &mut SignalFd,
) -> Result<Ended, String> {
    mounts::service(ROOT, config)?;
    enter(ROOT).map_err(failed("entering the service root"))?;
    ready::set();
    if config.role == Role::Install {
        match install_job {
            Ok(job) => install::start(&job),
            Err(e) => {
                say(&e);
                install::start_failed();
            }
        }
    }
    run::idle(signals)
}

/// Makes `root` the root of the guest's mount namespace, as a container runtime does, and
/// detaches the boot drive beneath it: every process from here on, the entry's and a file
/// session's worker, is in that root and in no chroot, which the kernel would refuse a worker's
/// user namespace in (§4.4 of docs/04-runtimes.md).
fn enter(root: &str) -> nix::Result<()> {
    use nix::mount::{MntFlags, MsFlags};
    let private = MsFlags::MS_REC | MsFlags::MS_PRIVATE;
    nix::mount::mount(None::<&str>, "/", None::<&str>, private, None::<&str>)?;
    nix::unistd::chdir(root)?;
    nix::unistd::pivot_root(".", ".")?;
    nix::mount::umount2(".", MntFlags::MNT_DETACH)?;
    nix::unistd::chdir("/")
}

/// Ends the guest: every other process, how the run ended on the drive, the volumes synced,
/// then a restart, which Firecracker takes as the end of its guest.
fn finish(ended: Ended, drive: Option<File>) -> ! {
    run::end_the_rest();
    if let Some(mut d) = drive
        && let Err(e) = ended.write(&mut d)
    {
        say(&format!("writing how the run ended: {e}"));
    }
    nix::unistd::sync();
    loop {
        let Err(e) = reboot(RebootMode::RB_AUTOBOOT);
        say(&format!("restarting: {e}"));
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn main() {
    let drive = OpenOptions::new().read(true).write(true).open(drive::CONFIG);
    let (ended, drive) = match drive {
        Ok(mut d) => {
            let ended = boot(&mut d).unwrap_or_else(|e| {
                say(&e);
                Ended::Failed
            });
            (ended, Some(d))
        }
        Err(e) => {
            say(&format!("opening the config drive {}: {e}", drive::CONFIG));
            (Ended::Failed, None)
        }
    };
    finish(ended, drive);
}
