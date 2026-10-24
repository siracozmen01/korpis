// SPDX-License-Identifier: AGPL-3.0-or-later

//! File sessions in the guest (§4.4 of docs/04-runtimes.md): on the files port, each connection
//! the host opens is one session, served by `korpis-fsworker` from the boot drive, the worker the
//! node's helper runs beside a container, here run by the first process with the connection as
//! its session. The host's first frame is the worker's spec, which this process passes on
//! unread; the worker builds its own root of the volumes, beneath a user namespace that maps the
//! guest's own ids, confines itself, and answers the session until the host closes it. The
//! worker's binary is held open from before the first process entered the workload's root, which
//! does not hold it.

use korpis_guest::control;
use rustix::net::{AddressFamily, SocketType};
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Stdio};
use std::sync::Arc;

/// Where the worker builds its root, in its own mount namespace: an empty directory only root
/// reaches.
const ROOT: &str = "/run/.korpis-files";
/// `VMADDR_CID_ANY`: listen whatever the guest's address.
const ANY_CID: u32 = u32::MAX;

/// Listens on the files port, with `worker` the binary each session runs, in a thread of its own;
/// a session waits for the workload's root ([`crate::ready`]) before its worker starts.
///
/// # Errors
/// The port cannot be made.
pub fn serve(worker: File) -> Result<(), String> {
    let listener = rustix::net::socket(AddressFamily::VSOCK, SocketType::STREAM, None)
        .map_err(|e| format!("a vsock socket: {e}"))?;
    let at = nix::sys::socket::VsockAddr::new(ANY_CID, control::FILES_PORT);
    nix::sys::socket::bind(listener.as_raw_fd(), &at)
        .map_err(|e| format!("the files port: {e}"))?;
    rustix::net::listen(&listener, 8).map_err(|e| format!("the files port: {e}"))?;
    let worker = Arc::new(worker);
    std::thread::spawn(move || accepting(&listener, &worker));
    Ok(())
}

/// Accepts sessions on `listener` for as long as the guest runs, each in a thread of its own.
fn accepting(listener: &OwnedFd, worker: &Arc<File>) {
    loop {
        let Ok(connection) = rustix::net::accept(listener) else { continue };
        let worker = Arc::clone(worker);
        std::thread::spawn(move || {
            if let Err(e) = session(connection, &worker) {
                crate::say(&format!("a file session: {e}"));
            }
        });
    }
}

/// Starts the worker for the session on `connection`, its spec the host's first frame.
fn session(connection: OwnedFd, worker: &File) -> io::Result<()> {
    let mut stream = File::from(connection);
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_le_bytes(length)).unwrap_or(usize::MAX);
    if length > control::MOST_SPEC {
        return Err(io::Error::other("a spec longer than the most"));
    }
    let mut spec = vec![0; length];
    stream.read_exact(&mut spec)?;
    if !crate::ready::wait() {
        return Err(io::Error::other("the workload's root is not ready"));
    }
    std::fs::create_dir_all(ROOT)?;
    std::fs::set_permissions(ROOT, std::fs::Permissions::from_mode(0o700))?;
    let program = format!("/proc/self/fd/{}", worker.as_raw_fd());
    let mut child = crate::exec::starting(|| {
        Command::new(&program)
            .env_clear()
            .env("RUST_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(OwnedFd::from(stream)))
            .spawn()
    })?;
    // The spec, then the end of the worker's input; the first process reaps it when it ends.
    let mut input = child.stdin.take().ok_or_else(|| io::Error::other("no input"))?;
    input.write_all(&spec)
}
