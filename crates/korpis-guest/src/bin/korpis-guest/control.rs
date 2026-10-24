// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control port, which the first process serves the driver over the microVM's vsock device (§6
//! of docs/04-runtimes.md): a thread that accepts, and one for each connection, at most eight at
//! once, each answering frames until the host closes it or is silent for half a minute. A ping is
//! answered with the protocol's version and how long the agent has run; a signal named is sent to
//! the entry's process group, if it is one the driver declares; a resize sets an interactive
//! entry's terminal to the size asked; a command is run as the entry runs (`exec`); the relays
//! beside the workload's identity socket are served as the host says (`relay`); and the workload's
//! files are written as the host sends them (`files`). A guest whose kernel has no vsock, or whose
//! device is missing, boots all the same, and says so: its `guest_agent` probe then fails, which
//! is what such a guest is.

use korpis_guest::config::Config;
use korpis_guest::control::{self, Kind, Pong, Resized, Signalled};
use rustix::net::{AddressFamily, SocketType};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// The most connections answered at once; more wait to be accepted.
const MOST_CONNECTIONS: usize = 8;
/// How long a connection may be silent before it is closed.
const IDLE: Duration = Duration::from_secs(30);
/// `VMADDR_CID_ANY`: listen whatever the guest's address.
const ANY_CID: u32 = u32::MAX;

/// Listens on the control port and answers in threads of its own; `started` is when the agent
/// began.
///
/// # Errors
/// The port cannot be listened on: no vsock in the guest's kernel, or no device.
pub fn serve(started: Instant, config: Arc<Config>) -> Result<(), String> {
    let listener = rustix::net::socket(AddressFamily::VSOCK, SocketType::STREAM, None)
        .map_err(|e| format!("a vsock socket: {e}"))?;
    let at = nix::sys::socket::VsockAddr::new(ANY_CID, control::PORT);
    nix::sys::socket::bind(listener.as_raw_fd(), &at)
        .map_err(|e| format!("the control port: {e}"))?;
    rustix::net::listen(&listener, 16).map_err(|e| format!("the control port: {e}"))?;
    std::thread::spawn(move || accepting(&listener, started, &config));
    Ok(())
}

/// Accepts connections on `listener` for as long as the guest runs; `config` is what the guest
/// runs, whose entry a command is run as.
fn accepting(listener: &OwnedFd, started: Instant, config: &Arc<Config>) {
    let open = Arc::new(AtomicUsize::new(0));
    loop {
        while open.load(Ordering::Acquire) >= MOST_CONNECTIONS {
            std::thread::sleep(Duration::from_millis(50));
        }
        let Ok(connection) = rustix::net::accept(listener) else { continue };
        open.fetch_add(1, Ordering::AcqRel);
        let (open, config) = (Arc::clone(&open), Arc::clone(config));
        std::thread::spawn(move || {
            answer(connection, started, &config);
            open.fetch_sub(1, Ordering::AcqRel);
        });
    }
}

/// Sends the entry's process group the signal `name` names, if it is one of [`control::SIGNALS`].
fn signal(name: &[u8]) -> Signalled {
    use std::str::FromStr as _;
    let name = std::str::from_utf8(name).ok().filter(|n| n.len() <= control::LONGEST_SIGNAL);
    let Some(signal) = name
        .filter(|n| control::SIGNALS.contains(n))
        .and_then(|n| nix::sys::signal::Signal::from_str(n).ok())
    else {
        return Signalled::Unknown;
    };
    let Some(entry) = crate::run::entry() else { return Signalled::NoEntry };
    match nix::sys::signal::killpg(entry, signal) {
        Ok(()) => Signalled::Sent,
        Err(_) => Signalled::NoEntry,
    }
}

/// Sets the console's size to the columns and rows `payload` holds, for an interactive entry,
/// whose terminal it is: the first process's own output is the same terminal, and the kernel tells
/// the entry's group with `SIGWINCH`.
fn resize(payload: &[u8], interactive: bool) -> Resized {
    let Some((cols, rows)) = control::size_of(payload) else { return Resized::Failed };
    if !interactive {
        return Resized::NoTerminal;
    }
    let size = rustix::termios::Winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    match rustix::termios::tcsetwinsize(std::io::stdout(), size) {
        Ok(()) => Resized::Set,
        Err(_) => Resized::Failed,
    }
}

/// Answers the frames on `connection` until it ends, is silent too long, or sends what this
/// agent does not take.
fn answer(connection: OwnedFd, started: Instant, config: &Config) {
    use rustix::net::sockopt::{Timeout, set_socket_timeout};
    if set_socket_timeout(&connection, Timeout::Recv, Some(IDLE)).is_err() {
        return;
    }
    let mut stream = std::fs::File::from(connection);
    while let Ok((kind, payload)) = control::read(&mut stream) {
        let answered = match kind {
            Kind::Ping => {
                let uptime_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let pong = Pong { version: control::VERSION, uptime_ms };
                control::write(&mut stream, Kind::Pong, &pong.encode())
            }
            Kind::Signal => {
                let outcome = signal(&payload);
                control::write(&mut stream, Kind::Signalled, &outcome.encode())
            }
            Kind::Resize => {
                let outcome = resize(&payload, config.interactive);
                control::write(&mut stream, Kind::Resized, &outcome.encode())
            }
            Kind::Exec => crate::exec::exec(&mut stream, &payload, config),
            Kind::Relays => {
                let Some(wanted) = control::Relays::decode(&payload) else { return };
                let served = crate::relay::relays(wanted);
                control::write(&mut stream, Kind::Relayed, &served.encode())
            }
            Kind::Put => {
                let refused = !crate::files::put(&payload);
                control::write(&mut stream, Kind::Stored, &[u8::from(refused)])
            }
            Kind::Keep => {
                let refused = !crate::files::keep(&payload);
                control::write(&mut stream, Kind::Kept, &[u8::from(refused)])
            }
            // Asked only of a guest booted for an install.
            Kind::InstallStatus if config.role == korpis_guest::config::Role::Install => {
                control::write(&mut stream, Kind::Installing, &crate::install::status().encode())
            }
            Kind::InstallStatus
            | Kind::Installing
            | Kind::Pong
            | Kind::Signalled
            | Kind::Resized
            | Kind::Output
            | Kind::Exited
            | Kind::Relayed
            | Kind::Stored
            | Kind::Kept => return,
        };
        if answered.is_err() {
            return;
        }
    }
}
