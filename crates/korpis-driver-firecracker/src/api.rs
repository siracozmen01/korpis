// SPDX-License-Identifier: AGPL-3.0-or-later

//! The one request the driver makes of a running Firecracker, over its API socket: Ctrl+Alt+Del,
//! which is how a stop reaches the guest (§6 of docs/04-runtimes.md). One HTTP/1.1 request on a
//! connection of its own, read back as far as its status line; Firecracker answers 204.
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

/// The request, whole.
fn request() -> String {
    let body = r#"{"action_type":"SendCtrlAltDel"}"#;
    format!(
        "PUT /actions HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n\
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

async fn send(socket: &Path) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;
    let fd = socket_at(socket)?;
    let at = format!("/proc/self/fd/{}", fd.as_raw_fd());
    let mut stream = tokio::net::UnixStream::connect(at).await?;
    drop(fd);
    stream.write_all(request().as_bytes()).await?;
    let mut status = [0u8; 12];
    stream.read_exact(&mut status).await?;
    if succeeded(&status) {
        return Ok(());
    }
    Err(io::Error::other(format!("Firecracker answered {}", String::from_utf8_lossy(&status))))
}

/// Sends the guest whose API is at `socket` Ctrl+Alt+Del.
pub async fn ctrl_alt_del(socket: &Path) -> io::Result<()> {
    tokio::time::timeout(ANSWER, send(socket))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Firecracker did not answer"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_whole_and_only_a_success_counts() {
        let r = request();
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
