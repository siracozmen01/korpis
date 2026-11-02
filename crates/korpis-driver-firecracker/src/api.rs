// SPDX-License-Identifier: AGPL-3.0-or-later

//! The requests the driver makes of a Firecracker, over its API socket: Ctrl+Alt+Del, which is how
//! a stop reaches the guest, and pausing, snapshotting and loading a snapshot, which is how a
//! checkpoint is taken and resumed. Each is one HTTP/1.1 request on a connection of its own, read
//! back as far as its status line; Firecracker answers 204.
//!
//! The socket is in the guest's chroot, which its Firecracker can write: one that a guest had
//! escaped into could put a link there to any socket on the host, and the driver is root. So the
//! socket is opened as itself, never through a link, checked to be a socket, and connected to
//! through that descriptor.

use std::io;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// How long Firecracker is given to answer.
const ANSWER: Duration = Duration::from_secs(5);
/// How long it is given to write or load a snapshot, which is as long as the guest's memory.
pub const SNAPSHOT: Duration = Duration::from_secs(600);

/// The request `method path` with `body`, whole.
fn request(method: &str, path: &str, body: &str) -> String {
    format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
         Accept: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// Whether `status`, the first bytes of an answer, is a success.
fn succeeded(status: &[u8]) -> bool {
    status.starts_with(b"HTTP/1.1 2")
}

/// The socket at `path`, opened without following a link, and checked to be one.
pub(crate) fn socket_at(path: &Path) -> io::Result<std::os::fd::OwnedFd> {
    use rustix::fs::{FileType, Mode, OFlags};
    let flags = OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = rustix::fs::open(path, flags, Mode::empty())?;
    if FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode) != FileType::Socket {
        return Err(io::Error::other(format!("{} is not a socket", path.display())));
    }
    Ok(fd)
}

async fn send(socket: &Path, request: &str) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    let fd = socket_at(socket)?;
    let at = format!("/proc/self/fd/{}", fd.as_raw_fd());
    let mut stream = tokio::net::UnixStream::connect(at).await?;
    drop(fd);
    stream.write_all(request.as_bytes()).await?;
    let mut status = [0u8; 12];
    stream.read_exact(&mut status).await?;
    if succeeded(&status) {
        return Ok(());
    }
    // Its answer says why, after its headers; read no further than a reason runs.
    let mut said = Vec::new();
    let _ = stream.take(4096).read_to_end(&mut said).await;
    let said = String::from_utf8_lossy(&said);
    let why = said.split_once("\r\n\r\n").map_or("", |(_, body)| body);
    let status = String::from_utf8_lossy(&status);
    Err(io::Error::other(format!("Firecracker answered {status}: {why}")))
}

/// Asks the Firecracker whose API is at `socket` `method path` with `body`, within `bound`.
async fn call(
    socket: &Path,
    (method, path): (&str, &str),
    body: &str,
    bound: Duration,
) -> io::Result<()> {
    tokio::time::timeout(bound, send(socket, &request(method, path, body)))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Firecracker did not answer"))?
}

/// Sends the guest whose API is at `socket` Ctrl+Alt+Del.
pub async fn ctrl_alt_del(socket: &Path) -> io::Result<()> {
    call(socket, ("PUT", "/actions"), r#"{"action_type":"SendCtrlAltDel"}"#, ANSWER).await
}

/// Pauses the guest whose API is at `socket` and writes its whole state into `vmstate` and its
/// memory into `memory`, both paths as its Firecracker sees them. It stays paused.
pub async fn snapshot(socket: &Path, vmstate: &str, memory: &str) -> io::Result<()> {
    call(socket, ("PATCH", "/vm"), r#"{"state":"Paused"}"#, ANSWER).await?;
    let body = serde_json::json!({"snapshot_type": "Full", "snapshot_path": vmstate,
                                  "mem_file_path": memory});
    call(socket, ("PUT", "/snapshot/create"), &body.to_string(), SNAPSHOT).await
}

/// Resumes the paused guest whose API is at `socket`.
pub async fn resume(socket: &Path) -> io::Result<()> {
    call(socket, ("PATCH", "/vm"), r#"{"state":"Resumed"}"#, ANSWER).await
}

/// Has the Firecracker whose API is at `socket`, configured with nothing, load the state in
/// `vmstate` and `memory`, as it sees them, and resume the guest.
pub async fn load(socket: &Path, vmstate: &str, memory: &str) -> io::Result<()> {
    let body = serde_json::json!({"snapshot_path": vmstate, "resume_vm": true,
                                  "mem_backend": {"backend_type": "File", "backend_path": memory}});
    call(socket, ("PUT", "/snapshot/load"), &body.to_string(), SNAPSHOT).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_whole_and_only_a_success_counts() {
        let r = request("PUT", "/actions", r#"{"action_type":"SendCtrlAltDel"}"#);
        assert!(r.starts_with("PUT /actions HTTP/1.1\r\n") && r.ends_with("\"SendCtrlAltDel\"}"));
        assert!(r.contains("Content-Length: 32\r\n"));
        assert!(succeeded(b"HTTP/1.1 204") && !succeeded(b"HTTP/1.1 400"));
    }

    #[tokio::test]
    async fn a_link_where_the_socket_should_be_is_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("elsewhere.socket");
        let _listening = std::os::unix::net::UnixListener::bind(&real).unwrap();
        let link = dir.path().join("api.socket");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(socket_at(&link).is_err(), "a link is refused");
        assert!(socket_at(&real).is_ok());
        let file = dir.path().join("file");
        std::fs::write(&file, b"").unwrap();
        assert!(socket_at(&file).is_err(), "so is what is not a socket");
    }
}
