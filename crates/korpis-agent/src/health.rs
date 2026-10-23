// SPDX-License-Identifier: AGPL-3.0-or-later

//! One attempt of a health probe (§5 of docs/04-runtimes.md): a `tcp` probe passes when a
//! connection to the endpoint's port is made, and an `http` probe when a `GET` of its path is
//! answered with the status it expects, or any 2xx where it expects none; a `query` probe is an
//! exchange its health provider directs, round by round, through the control plane (§5 of
//! docs/16-extensions.md). Each is made from a socket the helper opened inside the workload's
//! network namespace, so it reaches the endpoint whatever the endpoint's own rules turn away, and
//! what the workload is given, connecting included, is held to the probe's timeout. Whether the
//! attempts add up to a verdict is decided elsewhere (`korpis_core::workload::health`).

mod queries;
mod query;

pub use queries::Queries;
pub use query::Query;

use std::net::SocketAddr;
use std::os::fd::OwnedFd;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// The most of an HTTP answer read for its status line.
const STATUS_LINE: usize = 1024;

/// What one attempt does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    /// Connects to the address.
    Tcp(SocketAddr),
    /// Asks the address for `path` over HTTP/1.0 and reads the status of the answer.
    Http {
        /// Where.
        at: SocketAddr,
        /// The path asked for.
        path: String,
        /// The status expected; zero for any 2xx.
        expect: u16,
    },
    /// Runs the exchange the query probe's health provider directs.
    Query(Query),
    /// Runs the command inside the workload, through its driver, and passes on an exit status of
    /// zero; made from no socket of the prober's.
    Exec {
        /// The command and its arguments.
        command: Vec<String>,
    },
    /// Asks, through its driver, whether the agent inside the workload's guest answers; made from
    /// no socket of the prober's.
    GuestAgent,
    /// Asks when the workload last said on its identity socket that it is alive; made from no
    /// socket of the prober's.
    Heartbeat {
        /// The longest silence that is still healthy.
        max_interval: Duration,
    },
}

impl Attempt {
    /// Its kind, as a probe names it.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Tcp(_) => "tcp",
            Self::Http { .. } => "http",
            Self::Query(_) => "query",
            Self::Exec { .. } => "exec",
            Self::GuestAgent => "guest_agent",
            Self::Heartbeat { .. } => "heartbeat",
        }
    }

    /// Whether it is made from a datagram socket.
    #[must_use]
    pub const fn udp(&self) -> bool {
        matches!(self, Self::Query(q) if q.udp)
    }
}

/// What an attempt found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tried {
    /// It passed.
    Passed,
    /// It failed, or the workload did not answer in time.
    Failed,
    /// It could not be judged: a query probe's health provider, or the control plane, did not
    /// say (§5 of docs/16-extensions.md).
    Unjudged,
}

/// The status an HTTP answer starts with, if it starts with one.
fn status(head: &[u8]) -> Option<u16> {
    let line = head.split(|b| *b == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut words = line.split_whitespace();
    words.next().filter(|v| v.starts_with("HTTP/"))?;
    words.next()?.parse().ok()
}

/// Whether `got` is the status `expect` asks for.
const fn expected(got: u16, expect: u16) -> bool {
    if expect == 0 { got >= 200 && got < 300 } else { got == expect }
}

/// Makes the `tcp` or `http` attempt `what` from `socket`; whether it passed.
async fn tried(socket: OwnedFd, what: &Attempt) -> std::io::Result<bool> {
    let std = std::net::TcpStream::from(socket);
    std.set_nonblocking(true)?;
    let socket = tokio::net::TcpSocket::from_std_stream(std);
    let (at, asked) = match what {
        Attempt::Tcp(at) => (*at, None),
        Attempt::Http { at, path, expect } => (*at, Some((path, *expect))),
        // Run by `query`, and judged by the reconciler without a socket: never here.
        Attempt::Query(_)
        | Attempt::Exec { .. }
        | Attempt::GuestAgent
        | Attempt::Heartbeat { .. } => return Ok(false),
    };
    let mut stream = socket.connect(at).await?;
    let Some((path, expect)) = asked else { return Ok(true) };
    let request = format!(
        "GET {path} HTTP/1.0\r\nHost: {at}\r\nUser-Agent: korpis-probe\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await?;
    let mut head = Vec::with_capacity(STATUS_LINE);
    let mut chunk = [0_u8; 256];
    while head.len() < STATUS_LINE && !head.contains(&b'\n') {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(chunk.get(..n).unwrap_or_default());
    }
    Ok(status(&head).is_some_and(|got| expected(got, expect)))
}

/// Makes the `tcp` or `http` attempt `what` from `socket` within `timeout`; whether it passed.
/// One that fails, or does not finish in time, has not passed.
async fn passed(socket: OwnedFd, what: &Attempt, timeout: Duration) -> bool {
    match tokio::time::timeout(timeout, tried(socket, what)).await {
        Ok(Ok(passed)) => passed,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, kind = what.kind(), "a probe's attempt failed");
            false
        }
        Err(_) => false,
    }
}

/// Makes the attempt `what` from `socket`, the workload given `timeout`; a query probe asks its
/// rounds of the control plane through `queries`.
pub async fn attempt(
    socket: OwnedFd,
    what: &Attempt,
    timeout: Duration,
    queries: &Queries,
) -> Tried {
    if let Attempt::Query(q) = what {
        return query::exchange(socket, q, timeout, queries).await;
    }
    if passed(socket, what, timeout).await { Tried::Passed } else { Tried::Failed }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use tokio::net::TcpListener;

    fn socket() -> OwnedFd {
        let (inet, stream) = (rustix::net::AddressFamily::INET, rustix::net::SocketType::STREAM);
        rustix::net::socket_with(inet, stream, rustix::net::SocketFlags::CLOEXEC, None).unwrap()
    }

    /// A server on loopback answering every request with `answer`; its address.
    async fn answering(answer: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let mut asked = [0_u8; 512];
                let _ = s.read(&mut asked).await;
                let _ = s.write_all(answer.as_bytes()).await;
            }
        });
        at
    }

    fn http(at: SocketAddr, expect: u16) -> Attempt {
        Attempt::Http { at, path: "/alive".into(), expect }
    }

    #[tokio::test]
    async fn an_http_probe_passes_on_the_status_it_expects() {
        let second = Duration::from_secs(1);
        let ok = answering("HTTP/1.1 204 No Content\r\n\r\n").await;
        assert!(passed(socket(), &http(ok, 0), second).await, "any 2xx");
        assert!(passed(socket(), &http(ok, 204), second).await);
        assert!(!passed(socket(), &http(ok, 200), second).await, "not the one expected");
        let missing = answering("HTTP/1.0 404 Not Found\r\n\r\n").await;
        assert!(!passed(socket(), &http(missing, 0), second).await);
        let garbled = answering("hello\n").await;
        assert!(!passed(socket(), &http(garbled, 0), second).await, "not HTTP at all");
    }

    #[tokio::test]
    async fn a_tcp_probe_passes_on_a_connection_and_fails_on_none_or_silence() {
        let second = Duration::from_millis(300);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open = listener.local_addr().unwrap();
        assert!(passed(socket(), &Attempt::Tcp(open), second).await);
        drop(listener);
        assert!(!passed(socket(), &Attempt::Tcp(open), second).await, "nothing listens");

        // Accepted and never answered: the HTTP probe is held to its timeout.
        let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = silent.local_addr().unwrap();
        let started = std::time::Instant::now();
        assert!(!passed(socket(), &http(at, 0), second).await);
        assert!(started.elapsed() < Duration::from_secs(2), "it gave up at its timeout");
    }
}
