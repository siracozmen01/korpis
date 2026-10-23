// SPDX-License-Identifier: AGPL-3.0-or-later

//! The guest's agent, as the driver reaches it: over the microVM's vsock device, whose socket is
//! in the guest's chroot (§6 of docs/04-runtimes.md). A connection on that socket is told which
//! port of the guest it is for, `CONNECT <port>`, and Firecracker answers `OK <port>` once the
//! guest has accepted it; what follows is the control port's frames
//! (`korpis_guest::control`). The socket is opened as the API socket is, never through a link,
//! since the guest's Firecracker can write the chroot; and everything the guest says is bounded
//! before it is read and given a deadline, since the guest is the tenant's.

use crate::api::socket_at;
use korpis_guest::control::{self, HEADER, Kind, Pong};
use std::io;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;

/// The longest line Firecracker answers a `CONNECT` with.
const LONGEST_ANSWER: usize = 32;

/// A connection to the guest's control port through the device's socket at `socket`.
async fn connect(socket: &Path) -> io::Result<UnixStream> {
    use std::os::fd::AsRawFd as _;
    let fd = socket_at(socket)?;
    let mut stream = UnixStream::connect(format!("/proc/self/fd/{}", fd.as_raw_fd())).await?;
    drop(fd);
    stream.write_all(format!("CONNECT {}\n", control::PORT).as_bytes()).await?;
    let mut line = Vec::with_capacity(LONGEST_ANSWER);
    let mut byte = [0_u8; 1];
    while line.len() < LONGEST_ANSWER {
        stream.read_exact(&mut byte).await?;
        if byte == *b"\n" {
            break;
        }
        line.extend_from_slice(&byte);
    }
    if !line.starts_with(b"OK ") {
        return Err(io::Error::other("the guest did not accept a connection to its agent"));
    }
    Ok(stream)
}

/// Reads one frame of `want`, at most `most` bytes long, from `stream`.
async fn frame(stream: &mut UnixStream, want: Kind, most: usize) -> io::Result<Vec<u8>> {
    let mut head = [0_u8; HEADER];
    stream.read_exact(&mut head).await?;
    let (kind, length) = control::header(head)?;
    if kind != want || length > most {
        return Err(io::Error::other("the guest's agent answered out of turn"));
    }
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload).await?;
    Ok(payload)
}

async fn pinged(socket: &Path) -> io::Result<Pong> {
    let mut stream = connect(socket).await?;
    stream.write_all(&control::frame(Kind::Ping, &[])?).await?;
    let payload = frame(&mut stream, Kind::Pong, 12).await?;
    let pong = Pong::decode(&payload).ok_or_else(|| io::Error::other("a malformed answer"))?;
    if pong.version != control::VERSION {
        let why = format!("the guest's agent speaks version {}", pong.version);
        return Err(io::Error::other(why));
    }
    Ok(pong)
}

/// Asks the agent of the guest whose vsock device's socket is at `socket` whether it answers,
/// within `within`; its answer.
///
/// # Errors
/// It did not answer in time, its guest refused the connection, or it answered otherwise than
/// this driver's protocol says.
pub async fn ping(socket: &Path, within: Duration) -> io::Result<Pong> {
    tokio::time::timeout(within, pinged(socket))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "the guest's agent did not answer"))?
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use tokio::net::UnixListener;

    /// Plays Firecracker's side of the device and a guest's agent, answering `answer` after the
    /// handshake; the socket's path.
    fn device(dir: &Path, handshake: &'static [u8], answer: Vec<u8>) -> std::path::PathBuf {
        let path = dir.join("vsock.socket");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut asked = [0_u8; 13];
            s.read_exact(&mut asked).await.unwrap();
            assert_eq!(&asked, b"CONNECT 1024\n");
            s.write_all(handshake).await.unwrap();
            let mut ping = [0_u8; HEADER];
            s.read_exact(&mut ping).await.unwrap();
            s.write_all(&answer).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        path
    }

    #[tokio::test]
    async fn an_agent_that_answers_in_its_version_is_heard_and_any_other_is_not() {
        let pong = Pong { version: control::VERSION, uptime_ms: 42 };
        let good = control::frame(Kind::Pong, &pong.encode()).unwrap();
        let second = Duration::from_secs(1);
        let dir = tempfile::tempdir().unwrap();
        let socket = device(dir.path(), b"OK 1073741824\n", good);
        assert_eq!(ping(&socket, second).await.unwrap(), pong);

        let old = Pong { version: control::VERSION + 1, uptime_ms: 42 };
        let dir = tempfile::tempdir().unwrap();
        let socket =
            device(dir.path(), b"OK 5\n", control::frame(Kind::Pong, &old.encode()).unwrap());
        assert!(ping(&socket, second).await.is_err(), "another version");
        let dir = tempfile::tempdir().unwrap();
        let socket = device(dir.path(), b"OK 5\n", Vec::new());
        assert!(ping(&socket, second).await.is_err(), "silence, within its time");
        let dir = tempfile::tempdir().unwrap();
        let socket = device(dir.path(), b"NOPE\n", Vec::new());
        assert!(ping(&socket, second).await.is_err(), "a connection the guest refused");
    }
}
