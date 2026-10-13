// SPDX-License-Identifier: AGPL-3.0-or-later

//! The helper's socket, served by its one thread. The socket comes from systemd as standard
//! input (`StandardInput=socket`, `Accept=no`), so the helper runs only while it is asked
//! something and exits once idle, or is bound by the helper itself where there is no systemd.
//! Each connection's peer credentials, as the kernel recorded them at `connect`, must name an
//! allowed user before a byte of it is read; each read has a deadline, so a peer that stops
//! writing holds the helper for that long and no longer.

use crate::frame;
use crate::helper::Helper;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use std::collections::BTreeSet;
use std::os::fd::AsFd as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

/// How long one read or write on a connection may take.
const DEADLINE: Duration = Duration::from_secs(30);

/// The listening socket systemd passed as standard input.
pub fn inherited() -> std::io::Result<UnixListener> {
    let fd = std::io::stdin().as_fd().try_clone_to_owned()?;
    rustix::net::sockopt::socket_type(&fd)?;
    Ok(UnixListener::from(fd))
}

/// The socket at `path`, bound here, replacing one a previous run left; owner and group only.
pub fn bound(path: &Path) -> std::io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}

fn admitted(stream: &UnixStream, allowed: &BTreeSet<u32>) -> bool {
    match rustix::net::sockopt::socket_peercred(stream) {
        Ok(cred) if allowed.contains(&cred.uid.as_raw()) => true,
        Ok(cred) => {
            tracing::warn!(
                uid = cred.uid.as_raw(),
                pid = cred.pid.as_raw_nonzero().get(),
                "refused"
            );
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "a connection without credentials was refused");
            false
        }
    }
}

/// Whether a connection arrives within `idle`.
fn arrives(listener: &UnixListener, idle: Duration) -> std::io::Result<bool> {
    let timeout = Timespec::try_from(idle).unwrap_or(Timespec { tv_sec: 60, tv_nsec: 0 });
    let mut fds = [PollFd::new(listener, PollFlags::IN)];
    loop {
        match poll(&mut fds, Some(&timeout)) {
            Ok(n) => return Ok(n > 0),
            Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

/// Answers every request on `stream` until the peer closes it or misses a deadline.
fn connection(mut stream: UnixStream, helper: &mut Helper) {
    if let Err(e) =
        stream.set_read_timeout(Some(DEADLINE)).and(stream.set_write_timeout(Some(DEADLINE)))
    {
        tracing::warn!(error = %e, "a connection could not be given a deadline");
        return;
    }
    loop {
        let request = match frame::read(&mut stream) {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(e) => {
                tracing::warn!(error = %e, "a connection ended mid-request");
                return;
            }
        };
        if let Err(e) = frame::write(&mut stream, &helper.handle(&request)) {
            tracing::warn!(error = %e, "an answer could not be sent");
            return;
        }
    }
}

/// Serves `helper` on `listener` to the users in `allowed`, one connection at a time; with
/// `idle`, returns once no connection has arrived for that long.
pub fn serve(
    listener: &UnixListener,
    allowed: &BTreeSet<u32>,
    helper: &mut Helper,
    idle: Option<Duration>,
) -> std::io::Result<()> {
    loop {
        if let Some(idle) = idle
            && !arrives(listener, idle)?
        {
            tracing::info!("idle; exiting until the next connection");
            return Ok(());
        }
        let (stream, _) = match listener.accept() {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if admitted(&stream, allowed) {
            connection(stream, helper);
        }
    }
}
