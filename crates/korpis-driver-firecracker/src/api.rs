// SPDX-License-Identifier: AGPL-3.0-or-later

//! The one request the driver makes of a running Firecracker, over its API socket: Ctrl+Alt+Del,
//! which is how a stop reaches the guest (§6 of docs/04-runtimes.md). One HTTP/1.1 request on a
//! connection of its own, read back as far as its status line; Firecracker answers 204.

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

async fn send(socket: &Path) -> io::Result<()> {
    let mut stream = tokio::net::UnixStream::connect(socket).await?;
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
}
