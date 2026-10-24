// SPDX-License-Identifier: AGPL-3.0-or-later

//! The entry, run as the config says: its own process group, its environment and nothing else,
//! its user and group, the console as its output; and for an interactive console, a session of
//! its own whose controlling terminal is the serial port, its input, output, and errors all
//! there, so the terminal's line discipline does for it what a container's terminal does. The
//! first process reaps every process that ends, its own child or an orphan the entry left, until
//! the entry itself has ended; a stop sends the entry's group `SIGTERM`, and a second ends every
//! process. While the entry runs, its process id is where the control port sends a signal named.

use crate::failed;
use korpis_guest::config::Config;
use korpis_guest::status::Ended;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{SigSet, Signal, kill, killpg};
use nix::sys::signalfd::SignalFd;
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};

/// The entry's process id while it runs, zero otherwise: what the control port signals by name.
static ENTRY: AtomicI32 = AtomicI32::new(0);

/// The entry's process group while it runs.
pub fn entry() -> Option<Pid> {
    Some(ENTRY.load(Ordering::Acquire)).filter(|p| *p > 0).map(Pid::from_raw)
}

#[expect(clippy::as_conversions, reason = "a signal's number, which the record carries")]
fn number(signal: Signal) -> i32 {
    signal as i32
}

/// Reaps every process that has ended; how the entry `entry` ended, if it has.
fn reap(entry: Pid) -> Result<Option<Ended>, String> {
    loop {
        match waitpid(None, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(p, code)) if p == entry => return Ok(Some(Ended::Exited(code))),
            Ok(WaitStatus::Signaled(p, s, _)) if p == entry => {
                return Ok(Some(Ended::Signalled(number(s))));
            }
            Ok(WaitStatus::StillAlive) | Err(Errno::ECHILD) => return Ok(None),
            Ok(_) => {}
            Err(e) => return Err(format!("reaping: {e}")),
        }
    }
}

/// How many stops arrived among the signals waiting on `signals`.
fn stops(signals: &mut SignalFd) -> Result<usize, String> {
    let mut asked = 0;
    while let Some(info) = signals.read_signal().map_err(failed("reading signals"))? {
        let signal = i32::try_from(info.ssi_signo).ok().and_then(|s| Signal::try_from(s).ok());
        if matches!(signal, Some(Signal::SIGINT | Signal::SIGTERM)) {
            asked += 1;
        }
    }
    Ok(asked)
}

/// The serial port, which an interactive entry has as its terminal.
const TTY: &str = "/dev/ttyS0";

/// Gives `command` the serial port as its input, output, and errors.
fn on_the_terminal(command: &mut Command) -> Result<(), String> {
    let tty = std::fs::OpenOptions::new().read(true).write(true).open(TTY);
    let tty = tty.map_err(failed("opening the console's terminal"))?;
    let copy = || tty.try_clone().map_err(failed("opening the console's terminal"));
    command.stdin(copy()?).stdout(copy()?).stderr(tty);
    Ok(())
}

fn spawn(config: &Config) -> Result<Pid, String> {
    let (program, args) = config.argv.split_first().ok_or("the config names no entry")?;
    let working = if config.working_dir.is_empty() { "/" } else { config.working_dir.as_str() };
    let mut command = Command::new(program);
    command.args(args).env_clear().envs(&config.env).current_dir(working);
    command.uid(config.uid).gid(config.gid);
    if config.interactive {
        on_the_terminal(&mut command)?;
    } else {
        command.process_group(0).stdin(Stdio::null());
    }
    let interactive = config.interactive;
    // The first process blocks the signals it reads from a descriptor, and a child inherits its
    // parent's mask across exec: the entry would never see SIGTERM. So the child clears it, and
    // an interactive entry starts its own session, the terminal on its input its controlling
    // one; a session leader's process group is its own, so a stop still reaches its group.
    #[allow(unsafe_code, reason = "only pre_exec runs code in the child before exec")]
    // SAFETY: the closure calls pthread_sigmask on a set built on the stack, setsid, and the
    // TIOCSCTTY ioctl on descriptor 0, each a single system call that is async-signal-safe,
    // allocates nothing, and takes no lock, as code between fork and exec must. Descriptor 0 is
    // open for the closure's whole run: the spawn sets the child's input before pre_exec runs.
    unsafe {
        command.pre_exec(move || {
            SigSet::empty().thread_set_mask().map_err(std::io::Error::from)?;
            if interactive {
                rustix::process::setsid()?;
                rustix::process::ioctl_tiocsctty(rustix::stdio::stdin())?;
            }
            Ok(())
        });
    }
    let child = command.spawn().map_err(|e| format!("starting {program}: {e}"))?;
    let pid = i32::try_from(child.id()).map_err(failed("the entry's process id"))?;
    Ok(Pid::from_raw(pid))
}

/// Runs the entry until it ends. The first stop that arrives on `signals` sends the entry's
/// group `SIGTERM`; the next ends every process. The grace between them is the host's, which
/// sends the second when its own runs out (§6 of docs/04-runtimes.md), so the guest keeps none.
pub fn run(config: &Config, signals: &mut SignalFd) -> Result<Ended, String> {
    let entry = spawn(config)?;
    ENTRY.store(entry.as_raw(), Ordering::Release);
    let mut stopped = 0;
    loop {
        if let Some(ended) = reap(entry)? {
            ENTRY.store(0, Ordering::Release);
            return Ok(ended);
        }
        let mut fds = [PollFd::new(signals.as_fd(), PollFlags::POLLIN)];
        match poll(&mut fds, PollTimeout::NONE) {
            Ok(_) | Err(Errno::EINTR) => {}
            Err(e) => return Err(format!("waiting: {e}")),
        }
        let asked = stops(signals)?;
        if asked > 0 && stopped == 0 {
            // The group may already be gone, which is what a stop wants.
            let _ = killpg(entry, Signal::SIGTERM);
        }
        stopped += asked;
        if stopped > 1 {
            let _ = kill(Pid::from_raw(-1), Signal::SIGKILL);
        }
    }
}

/// Ends every process but this one and reaps them all.
pub fn end_the_rest() {
    // -1 is every process this one may signal, which for the first process is all but itself.
    let _ = kill(Pid::from_raw(-1), Signal::SIGKILL);
    while waitpid(None, None).is_ok() {}
}
