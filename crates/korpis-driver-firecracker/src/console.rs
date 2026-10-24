// SPDX-License-Identifier: AGPL-3.0-or-later

//! A guest's console (§6 of docs/04-runtimes.md; §4 and §6 of docs/14-streams.md). The guest's
//! serial port is its Firecracker's standard input and output, and with no stream both are empty.
//! With one, each run starts `korpis-stream-writer` first, handing it the read end of a pipe whose
//! write end is the Firecracker's output, as containerd's shim hands it a container's; once the
//! guest's cgroup exists the writer is moved into it, so it is counted with its guest and outlives
//! a restart of the driver, and it ends when the guest's Firecracker does, having read what was
//! left. An interactive console's input is a FIFO the writer holds and feeds, which is the
//! Firecracker's standard input.

use command_fds::{CommandFdExt as _, FdMapping};
use korpis_driver_kit::console;
use korpis_proto::korpis::driver::v1 as driver;
use rustix::pipe::{PipeFlags, pipe_with};
use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// How long the writer is given to say it is ready.
const READY: Duration = Duration::from_secs(10);
/// The FIFO the driver sends framed input on.
const INPUT: &str = "console.input";
/// The FIFO the writer feeds, which is the guest's input.
const STDIN: &str = "console.stdin";
/// The secret values the writer redacts, readable by the driver's user alone.
const SECRETS: &str = "console.secrets";
/// What the writer says about itself.
const LOG: &str = "console.log";

/// A run's console: its Firecracker's standard input and output, and the writer to move into
/// its cgroup once there is one.
#[derive(Debug)]
pub struct Attached {
    /// The Firecracker's standard input.
    pub stdin: Stdio,
    /// Its standard output.
    pub stdout: Stdio,
    /// The writer, if there is one, held unreaped until it is moved, so its process id names it
    /// and no other process.
    pub writer: Option<tokio::process::Child>,
}

/// Prepares the console files in object directory `dir` for `stream`: the secrets the writer
/// redacts, and an interactive console's FIFOs.
pub fn prepare(dir: &Path, stream: &driver::StreamCapture) -> io::Result<()> {
    console::write_secrets(&dir.join(SECRETS), stream)?;
    if stream.input {
        for fifo in [INPUT, STDIN] {
            console::make_fifo(&dir.join(fifo))?;
        }
    }
    Ok(())
}

/// The writer `writer`'s command for `stream`, with the guest's output on its descriptor 3,
/// nothing on 4, and on 5 what it closes once it is ready.
fn command(
    writer: &Path,
    dir: &Path,
    stream: &driver::StreamCapture,
    [output, nothing, ready]: [OwnedFd; 3],
) -> io::Result<std::process::Command> {
    let mut c = std::process::Command::new(writer);
    for (flag, value) in console::writer_args(stream) {
        c.arg(flag).arg(value);
    }
    c.arg("--secrets").arg(dir.join(SECRETS));
    if stream.input {
        c.arg("--input").arg(dir.join(INPUT)).arg("--stdin").arg(dir.join(STDIN));
    }
    let mappings = [(output, 3), (nothing, 4), (ready, 5)]
        .map(|(parent_fd, child_fd)| FdMapping { parent_fd, child_fd });
    c.fd_mappings(mappings.into()).map_err(io::Error::other)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(LOG))?;
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(log).process_group(0);
    Ok(c)
}

/// The guest's input: the FIFO the writer feeds, opened without waiting, since the writer
/// already holds it, and then read as a plain descriptor.
fn input(dir: &Path) -> io::Result<Stdio> {
    use rustix::fs::{Mode, OFlags};
    let fd = rustix::fs::open(dir.join(STDIN), OFlags::RDONLY | OFlags::NONBLOCK, Mode::empty())?;
    rustix::fs::fcntl_setfl(&fd, OFlags::empty())?;
    Ok(Stdio::from(fd))
}

/// The console of a run of the object in `dir`, written to `stream` by the writer `writer`; an
/// empty one when there is no stream.
pub async fn attach(
    writer: &Path,
    dir: &Path,
    stream: Option<&driver::StreamCapture>,
) -> io::Result<Attached> {
    let Some(stream) = stream else {
        return Ok(Attached { stdin: Stdio::null(), stdout: Stdio::null(), writer: None });
    };
    let (output, guest_output) = pipe_with(PipeFlags::CLOEXEC)?;
    let (ready, said_ready) = pipe_with(PipeFlags::CLOEXEC)?;
    let nothing = OwnedFd::from(File::open("/dev/null")?);
    // The command, and the descriptors it holds, are gone once the writer has them.
    let mut child = {
        let c = command(writer, dir, stream, [output, nothing, said_ready])?;
        tokio::process::Command::from(c).spawn()?
    };
    let waited = tokio::task::spawn_blocking(move || File::from(ready).read(&mut [0u8; 1]));
    let closed = matches!(tokio::time::timeout(READY, waited).await, Ok(Ok(Ok(0))));
    if !closed || child.try_wait()?.is_some() {
        let _ = child.start_kill();
        let said = std::fs::read_to_string(dir.join(LOG)).unwrap_or_default();
        return Err(io::Error::other(format!("the console's writer did not start: {said}")));
    }
    let stdin = if stream.input { input(dir)? } else { Stdio::null() };
    Ok(Attached { stdin, stdout: Stdio::from(guest_output), writer: Some(child) })
}

/// Moves `writer` into its guest's cgroup at `cgroup`; the caller lets it go after. A child that
/// has not been waited for keeps its process id even once it has exited, so the id written is
/// the writer's and never a process that took its id after it; one already waited for has none,
/// and is not moved.
pub fn join(writer: &tokio::process::Child, cgroup: &Path) -> io::Result<()> {
    match writer.id() {
        Some(pid) => std::fs::write(cgroup.join("cgroup.procs"), pid.to_string()),
        None => Ok(()),
    }
}

/// Sends `data`, typed by `by` in `session`, to the console of the object in `dir`, written to
/// `stream`: a refusal if it takes no input, and otherwise what failed, as a caller is told.
pub fn send(
    dir: &Path,
    stream: Option<&driver::StreamCapture>,
    (by, session): (&str, &str),
    data: &[u8],
) -> Result<(), (bool, String)> {
    if !stream.is_some_and(|s| s.input) {
        return Err((true, "this console takes no input".into()));
    }
    for frame in console::frames(by, session, data).map_err(|e| (true, e))? {
        console::send(&dir.join(INPUT), &frame).map_err(|e| (false, e))?;
    }
    Ok(())
}

/// Records in the console of the object in `dir`, written to `stream`, that `by` in `session`
/// resized its terminal to `(cols, rows)`, as a container's resize is recorded: a refusal if it
/// takes no input, and otherwise what failed.
pub fn record_resize(
    dir: &Path,
    stream: Option<&driver::StreamCapture>,
    (by, session): (&str, &str),
    (cols, rows): (u16, u16),
) -> Result<(), (bool, String)> {
    if !stream.is_some_and(|s| s.input) {
        return Err((true, "this console takes no input".into()));
    }
    let frame = console::resize_frame(by, session, cols, rows);
    console::send(&dir.join(INPUT), &frame).map_err(|e| (false, e))
}
