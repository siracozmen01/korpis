// SPDX-License-Identifier: AGPL-3.0-or-later

//! A command the host asks for on the control port (§4.2 of docs/04-runtimes.md), run as the
//! entry runs: its user and group, its environment, its working directory, in the workload's
//! root, in a process group of its own. Its stdout and stderr are one pipe this thread reads and
//! sends on, at most the limit asked, what is past it read and dropped; past its timeout its group
//! is killed. The first process reaps every child, so a command's status is kept for it here by
//! the reaper and waited for, and its process id is registered before the reaper could see it.

use crate::run::{Output, spawn};
use korpis_guest::config::Config;
use korpis_guest::control::{self, Exec, Exited, Kind};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::signal::{Signal, killpg};
use nix::sys::wait::WaitStatus;
use nix::unistd::Pid;
use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd as _, OwnedFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How many commands are being started now.
static SPAWNING: AtomicUsize = AtomicUsize::new(0);

/// Whether a command is being started, while which the first process reaps nothing but the
/// entry: the standard library waits itself for a child whose exec failed, and must find it.
pub fn spawning() -> bool {
    SPAWNING.load(Ordering::Acquire) > 0
}

/// Runs `start`, which starts a child, as one being started: the first process reaps nothing but
/// the entry meanwhile, then reaps whatever ended.
pub fn starting<T>(start: impl FnOnce() -> T) -> T {
    SPAWNING.fetch_add(1, Ordering::AcqRel);
    let started = start();
    SPAWNING.fetch_sub(1, Ordering::AcqRel);
    let _ = nix::sys::signal::kill(Pid::this(), Signal::SIGCHLD);
    started
}

/// The commands running, by process id, each with its status once reaped.
static RUNNING: Mutex<BTreeMap<i32, Option<u32>>> = Mutex::new(BTreeMap::new());
/// The longest a command may run.
const MOST_TIMEOUT: Duration = Duration::from_secs(600);
/// The most of a command's output sent.
const MOST_OUTPUT: u32 = 64 * 1024;
/// How long a killed command's output is still read, and its status waited for.
const AFTER: Duration = Duration::from_secs(5);
/// The status of a command that could not be started, as a shell reports one.
const NOT_STARTED: u32 = 127;

fn running() -> MutexGuard<'static, BTreeMap<i32, Option<u32>>> {
    RUNNING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keeps `status`, which the first process reaped, for the command it ended, if it is one.
pub fn reaped(status: WaitStatus) {
    let (pid, code) = match status {
        WaitStatus::Exited(p, c) => (p, u32::try_from(c).unwrap_or(0)),
        WaitStatus::Signaled(p, s, _) => {
            (p, 128 + u32::try_from(crate::run::number(s)).unwrap_or(0))
        }
        _ => return,
    };
    if let Some(slot) = running().get_mut(&pid.as_raw()) {
        *slot = Some(code);
    }
}

/// Whether `asked` is within the bounds a command is run in.
fn bounded(asked: &Exec) -> bool {
    !asked.argv.is_empty()
        && asked.timeout_ms > 0
        && Duration::from_millis(asked.timeout_ms) <= MOST_TIMEOUT
        && asked.output_limit <= MOST_OUTPUT
}

/// Runs the command `payload` asks for, sending what it writes, then how it ended, on `stream`.
///
/// # Errors
/// The stream fails.
pub fn exec(stream: &mut File, payload: &[u8], config: &Config) -> io::Result<()> {
    let asked = serde_json::from_slice::<Exec>(payload).ok().filter(bounded);
    let Some(asked) = asked else { return refused(stream, "not a command this agent runs") };
    let (reader, writer) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
    let pid = {
        // Held across the spawn, so the reaper finds the command registered whenever it ends.
        let mut running = running();
        SPAWNING.fetch_add(1, Ordering::AcqRel);
        let spawned = spawn(&asked.argv, config, Output::Pipe(writer));
        SPAWNING.fetch_sub(1, Ordering::AcqRel);
        // The first process may have left a command that ended meanwhile; it reaps it now.
        let _ = nix::sys::signal::kill(Pid::this(), Signal::SIGCHLD);
        match spawned {
            Ok(pid) => {
                running.insert(pid.as_raw(), None);
                pid
            }
            Err(why) => {
                drop(running);
                return refused(stream, &why);
            }
        }
    };
    let deadline = Instant::now() + Duration::from_millis(asked.timeout_ms);
    let (timed_out, truncated) = forward(stream, &reader, pid, (deadline, asked.output_limit))?;
    let (status, timed_out) = ended(pid, deadline, timed_out);
    let exited = Exited { status: status.unwrap_or(128 + 9), timed_out, truncated };
    control::write(stream, Kind::Exited, &exited.encode())
}

/// Says why a command was not run, as its output, and ends it as one that could not start.
fn refused(stream: &mut File, why: &str) -> io::Result<()> {
    control::write(stream, Kind::Output, format!("korpis-guest: {why}\n").as_bytes())?;
    let exited = Exited { status: NOT_STARTED, timed_out: false, truncated: false };
    control::write(stream, Kind::Exited, &exited.encode())
}

/// Sends what the command `pid` writes on `reader` until every writer has closed it, at most the
/// limit asked; kills its group once it outlives its timeout, and stops reading a while after.
/// Whether it was killed, and whether output was dropped.
fn forward(
    stream: &mut File,
    reader: &OwnedFd,
    pid: Pid,
    (deadline, limit): (Instant, u32),
) -> io::Result<(bool, bool)> {
    let mut left = usize::try_from(limit).unwrap_or(0);
    let (mut timed_out, mut truncated) = (false, false);
    let mut until = deadline;
    let mut buffer = vec![0_u8; 8192];
    loop {
        let wait = until.saturating_duration_since(Instant::now());
        if wait.is_zero() {
            if timed_out {
                return Ok((true, truncated));
            }
            timed_out = true;
            let _ = killpg(pid, Signal::SIGKILL);
            until = Instant::now() + AFTER;
            continue;
        }
        let mut fds = [PollFd::new(reader.as_fd(), PollFlags::POLLIN)];
        let timeout = PollTimeout::try_from(wait).unwrap_or(PollTimeout::MAX);
        if poll(&mut fds, timeout).is_err_and(|e| e != nix::errno::Errno::EINTR) {
            return Ok((timed_out, truncated));
        }
        if fds.first().and_then(PollFd::revents).is_none_or(|r| r.is_empty()) {
            continue;
        }
        let n = match rustix::io::read(reader, &mut buffer) {
            Ok(0) | Err(_) => return Ok((timed_out, truncated)),
            Ok(n) => n,
        };
        let read = buffer.get(..n).unwrap_or_default();
        let sent = read.get(..n.min(left)).unwrap_or_default();
        truncated |= sent.len() < n;
        left -= sent.len();
        if !sent.is_empty() {
            control::write(stream, Kind::Output, sent)?;
        }
    }
}

/// The status of the command `pid` once the reaper has kept it, waited for until its `deadline`,
/// when its group is killed, and a while after; whether it was killed for its timeout. It is
/// forgotten after: one that outlives even that is reported as killed.
fn ended(pid: Pid, deadline: Instant, timed_out: bool) -> (Option<u32>, bool) {
    let status = || running().get(&pid.as_raw()).copied().flatten();
    let mut timed_out = timed_out;
    let mut until = if timed_out { Instant::now() + AFTER } else { deadline };
    let found = loop {
        if let Some(s) = status() {
            break Some(s);
        }
        if Instant::now() >= until {
            if timed_out {
                break None;
            }
            timed_out = true;
            let _ = killpg(pid, Signal::SIGKILL);
            until = Instant::now() + AFTER;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    running().remove(&pid.as_raw());
    (found, timed_out)
}
