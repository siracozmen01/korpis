// SPDX-License-Identifier: AGPL-3.0-or-later

//! `korpis-guest`, the first process of every microVM (§4.4 and §6 of docs/04-runtimes.md). The
//! kernel mounts the boot drive as its root and starts it. It reads its config drive, assembles
//! the workload's root (the image beneath a small writable layer, made read-only once its mount
//! points exist, as a container's root is), mounts the volumes, sets `eth0`, and runs the entry
//! as its user, reaping whatever the entry's own children leave behind. A stop arrives as
//! Ctrl+Alt+Del and reaches the entry as `SIGTERM`; a second ends every process. When the entry
//! has ended, it ends every other process, writes how the run ended on the config drive, and
//! restarts the guest, which ends the microVM: Firecracker exits when its guest reboots. The first process must never return, so every failure ends the same way, with
//! the reason on the console.
//!
//! It holds one `unsafe` block, in `run`: the entry's child clears the signal mask it inherits
//! before exec, which only `pre_exec` can do.

mod mounts;
mod net;
mod run;

use korpis_guest::config::Config;
use korpis_guest::drive;
use korpis_guest::status::Ended;
use nix::sys::reboot::{RebootMode, reboot, set_cad_enabled};
use nix::sys::signal::{SigSet, Signal};
use nix::sys::signalfd::{SfdFlags, SignalFd};
use std::fs::{File, OpenOptions};
use std::time::Duration;

/// Where the workload's root is assembled, on the boot drive.
const ROOT: &str = "/newroot";

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

/// Everything up to the entry's end, on the config drive `drive`.
fn boot(drive: &mut File) -> Result<Ended, String> {
    // Ctrl+Alt+Del becomes SIGINT to this process rather than an immediate restart, so a stop
    // reaches the entry first.
    set_cad_enabled(false).map_err(failed("taking Ctrl+Alt+Del"))?;
    let mut signals = signals()?;
    let config = Config::read(drive).map_err(failed("reading the config drive"))?;
    nix::unistd::sethostname(&config.hostname).map_err(failed("setting the hostname"))?;
    mounts::root(ROOT, &config)?;
    net::configure(config.network.as_ref())?;
    std::os::unix::fs::chroot(ROOT).map_err(failed("entering the workload's root"))?;
    nix::unistd::chdir("/").map_err(failed("entering the workload's root"))?;
    run::run(&config, &mut signals)
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
