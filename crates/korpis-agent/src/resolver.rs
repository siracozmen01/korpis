// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's resolver (§5.1 of docs/07-networking.md), answered by the supervisor on the
//! sockets the helper opened inside the workload's own network namespace, on the address its
//! `resolv.conf` names alone. A query never leaves the namespace, so no firewall of the host's
//! stands between the workload and its resolver; the socket a query arrives on is the workload
//! that asked, which is held to its own rate; and each is forwarded, over UDP or TCP as it came,
//! to the resolvers the host's own `resolv.conf` names, which the host reaches whatever they
//! are. The node talks to its provider's resolver, and the workload never does, so the metadata
//! address stays denied to it.

mod bucket;
pub mod message;
#[cfg(test)]
mod tests;

use bucket::Bucket;
use std::io;
use std::net::SocketAddr;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// How long an upstream resolver is given to answer before the next is asked.
const UPSTREAM: Duration = Duration::from_secs(2);
/// How long a TCP connection from a workload may sit between queries.
const IDLE: Duration = Duration::from_secs(10);
const SERVFAIL: u8 = 2;
const REFUSED: u8 = 5;

/// The sockets the resolver answers on.
#[derive(Debug)]
pub struct Bound {
    udp: UdpSocket,
    tcp: TcpListener,
}

impl Bound {
    /// The resolver's sockets: `udp`, and `tcp` listening.
    pub fn new(udp: std::net::UdpSocket, tcp: std::net::TcpListener) -> io::Result<Self> {
        udp.set_nonblocking(true)?;
        tcp.set_nonblocking(true)?;
        Ok(Self { udp: UdpSocket::from_std(udp)?, tcp: TcpListener::from_std(tcp)? })
    }

    /// The sockets as the helper sends them: UDP, then TCP listening.
    pub fn from_attached(attached: Vec<OwnedFd>) -> io::Result<Self> {
        let mut fds = attached.into_iter();
        match (fds.next(), fds.next(), fds.next()) {
            (Some(udp), Some(tcp), None) => Self::new(udp.into(), tcp.into()),
            _ => Err(io::Error::other("a resolver is two sockets, UDP and TCP")),
        }
    }
}

/// The answer to `query` with `rcode` and nothing else.
fn failed(query: &[u8], rcode: u8) -> Option<Vec<u8>> {
    let mut answer = message::refused(query)?;
    if let Some(flags) = answer.get_mut(3) {
        *flags = 0x80 | rcode;
    }
    Some(answer)
}

/// The resolver.
#[derive(Debug, Clone)]
pub struct Resolver {
    conf: PathBuf,
    bucket: Arc<Mutex<Bucket>>,
    /// The upstreams' port: 53, except in a test.
    upstream_port: u16,
}

impl Resolver {
    /// One workload's resolver, forwarding to the resolvers `conf` names.
    #[must_use]
    pub fn new(conf: PathBuf) -> Self {
        Self { conf, bucket: Arc::default(), upstream_port: 53 }
    }

    /// Whether the workload is within its rate now, taking a token if so.
    fn admits(&self) -> bool {
        self.bucket.lock().is_ok_and(|mut b| b.take(Instant::now()))
    }

    async fn upstreams(&self) -> Vec<SocketAddr> {
        let conf = tokio::fs::read_to_string(&self.conf).await.unwrap_or_default();
        message::upstreams(&conf, self.upstream_port)
    }

    /// What the upstream resolvers answer `query` over UDP: the first answer bearing its
    /// identifier, from the first resolver that gives one in time.
    async fn over_udp(&self, query: &[u8]) -> Option<Vec<u8>> {
        let id = message::id(query)?;
        for upstream in self.upstreams().await {
            let local: SocketAddr = if upstream.is_ipv4() {
                (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
            } else {
                (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
            };
            let Ok(s) = UdpSocket::bind(local).await else { continue };
            if s.connect(upstream).await.is_err() || s.send(query).await.is_err() {
                continue;
            }
            if let Ok(Some(answer)) = tokio::time::timeout(UPSTREAM, answer_to(&s, id)).await {
                return Some(answer);
            }
        }
        None
    }

    /// What the upstream resolvers answer `query` over TCP.
    async fn over_tcp(&self, query: &[u8]) -> Option<Vec<u8>> {
        for upstream in self.upstreams().await {
            let asked = async {
                let mut s = TcpStream::connect(upstream).await.ok()?;
                exchange(&mut s, query).await
            };
            if let Ok(Some(answer)) = tokio::time::timeout(UPSTREAM, asked).await {
                return Some(answer);
            }
        }
        None
    }

    /// The answer for `query`, which [`Self::admits`] judged.
    async fn answer(&self, query: &[u8], within: bool, tcp: bool) -> Option<Vec<u8>> {
        if !within {
            return failed(query, REFUSED);
        }
        let forwarded = if tcp { self.over_tcp(query).await } else { self.over_udp(query).await };
        forwarded.or_else(|| failed(query, SERVFAIL))
    }

    /// Answers on `bound` until the task is dropped.
    pub async fn serve(self, bound: Bound) {
        let udp = Arc::new(bound.udp);
        let tcp = bound.tcp;
        tokio::join!(self.clone().serve_udp(udp), self.serve_tcp(tcp));
    }

    async fn serve_udp(self, udp: Arc<UdpSocket>) {
        let mut buf = vec![0; 65_535];
        loop {
            let Ok((n, from)) = udp.recv_from(&mut buf).await else { continue };
            let within = self.admits();
            let Some(query) = buf.get(..n).map(<[u8]>::to_vec) else { continue };
            tokio::spawn(self.clone().reply(Arc::clone(&udp), query, (from, within)));
        }
    }

    async fn serve_tcp(self, tcp: TcpListener) {
        loop {
            let Ok((s, _)) = tcp.accept().await else { continue };
            tokio::spawn(self.clone().converse(s));
        }
    }
}

impl Resolver {
    /// Answers `query` from `from` over `udp`.
    async fn reply(self, udp: Arc<UdpSocket>, query: Vec<u8>, (from, within): (SocketAddr, bool)) {
        if let Some(answer) = self.answer(&query, within, false).await {
            let _ = udp.send_to(&answer, from).await;
        }
    }

    /// Answers the queries sent on `s` until it is idle, closes, or cannot be written.
    async fn converse(self, mut s: TcpStream) {
        while let Ok(Some(query)) = tokio::time::timeout(IDLE, read(&mut s)).await {
            let within = self.admits();
            let Some(answer) = self.answer(&query, within, true).await else { break };
            if write(&mut s, &answer).await.is_none() {
                break;
            }
        }
    }
}

/// The first datagram on `s` bearing the identifier `id`.
async fn answer_to(s: &UdpSocket, id: [u8; 2]) -> Option<Vec<u8>> {
    let mut buf = vec![0; 65_535];
    loop {
        let n = s.recv(&mut buf).await.ok()?;
        let got = buf.get(..n)?;
        if message::id(got) == Some(id) {
            return Some(got.to_vec());
        }
    }
}

/// One message from a TCP stream, length first (RFC 1035 §4.2.2).
async fn read(s: &mut TcpStream) -> Option<Vec<u8>> {
    let len = s.read_u16().await.ok()?;
    let mut message = vec![0; usize::from(len)];
    s.read_exact(&mut message).await.ok()?;
    Some(message)
}

/// Writes `message` to a TCP stream, length first, in one write, so a reader is never handed the
/// length alone.
async fn write(s: &mut TcpStream, message: &[u8]) -> Option<()> {
    let len = u16::try_from(message.len()).ok()?;
    let mut framed = Vec::with_capacity(2 + message.len());
    framed.extend_from_slice(&len.to_be_bytes());
    framed.extend_from_slice(message);
    s.write_all(&framed).await.ok()
}

async fn exchange(s: &mut TcpStream, query: &[u8]) -> Option<Vec<u8>> {
    write(s, query).await?;
    read(s).await
}
