// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's API, relayed to an installed extension's workload (§9.5 of
//! docs/08-identity.md). The agent serves `api.sock` in the workload's own directory and copies
//! each connection on it, byte for byte, to the API at the address the node enrolled with, over
//! the host's network. The extension speaks TLS through it to the control plane itself, so the
//! relay carries what it cannot read. A workload's connections are bounded, so it cannot spend
//! the agent's descriptors.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio::sync::Semaphore;

/// The relay's name in a workload's directory.
pub const RELAY: &str = "api.sock";
/// How many connections one workload holds through its relay at once.
pub const MOST_CONNECTIONS: usize = 64;
/// How long reaching the control plane may take before a connection is given up.
const CONNECT: Duration = Duration::from_secs(10);

/// Binds `dir`'s relay socket, replacing one a previous run left, and relays it to `api`.
///
/// # Errors
/// The socket cannot be bound.
pub fn serve(dir: &Path, api: (String, u16)) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let listener = super::socket::bind(&dir.join(RELAY))?;
    Ok(tokio::spawn(accept(listener, Arc::new(api))))
}

async fn accept(listener: UnixListener, api: Arc<(String, u16)>) {
    let slots = Arc::new(Semaphore::new(MOST_CONNECTIONS));
    loop {
        let Ok(slot) = slots.clone().acquire_owned().await else { return };
        match listener.accept().await {
            Ok((stream, _)) => {
                let api = api.clone();
                tokio::spawn(async move {
                    relay(stream, &api).await;
                    drop(slot);
                });
            }
            Err(e) => {
                tracing::warn!(error = %e, "a relay socket stopped accepting");
                return;
            }
        }
    }
}

/// Copies one connection both ways until either side closes it.
async fn relay(mut inside: UnixStream, (host, port): &(String, u16)) {
    let reached = tokio::time::timeout(CONNECT, TcpStream::connect((host.as_str(), *port))).await;
    let mut outside = match reached {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!(%host, port, error = %e, "the relay could not reach the control plane");
            return;
        }
        Err(_) => {
            tracing::warn!(%host, port, "the relay timed out reaching the control plane");
            return;
        }
    };
    if let Err(e) = tokio::io::copy_bidirectional(&mut inside, &mut outside).await {
        tracing::debug!(error = %e, "a relayed connection ended");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// Answers each connection's five bytes reversed, every connection at once.
    async fn reverse(upstream: tokio::net::TcpListener) {
        while let Ok((s, _)) = upstream.accept().await {
            tokio::spawn(answer(s));
        }
    }

    async fn answer(mut s: TcpStream) {
        let mut buf = [0u8; 5];
        if s.read_exact(&mut buf).await.is_ok() {
            buf.reverse();
            let _ = s.write_all(&buf).await;
        }
    }

    #[tokio::test]
    async fn bytes_cross_both_ways_and_connections_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = upstream.local_addr().unwrap().port();
        tokio::spawn(reverse(upstream));
        let task = serve(dir.path(), ("127.0.0.1".into(), port)).unwrap();
        let mut c = UnixStream::connect(dir.path().join(RELAY)).await.unwrap();
        c.write_all(b"hello").await.unwrap();
        let mut back = [0u8; 5];
        c.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"olleh");

        let mut held = Vec::new();
        for _ in 0..MOST_CONNECTIONS {
            held.push(UnixStream::connect(dir.path().join(RELAY)).await.unwrap());
        }
        let mut over = UnixStream::connect(dir.path().join(RELAY)).await.unwrap();
        over.write_all(b"waits").await.unwrap();
        let mut none = [0u8; 5];
        let waited = tokio::time::timeout(Duration::from_millis(300), over.read_exact(&mut none));
        assert!(waited.await.is_err(), "one past the bound waits for a slot");
        drop(c);
        task.abort();
    }
}
