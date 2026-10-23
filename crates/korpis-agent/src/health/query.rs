// SPDX-License-Identifier: AGPL-3.0-or-later

//! A query probe's exchange (§5 of docs/04-runtimes.md, §5 of docs/16-extensions.md). The node
//! holds the socket and the health provider holds the protocol: the node connects to the endpoint
//! from inside the workload's namespace, asks the control plane what to send, sends it, reads what
//! comes back, and asks again with the round, until the provider gives its verdict. Over TCP every
//! round shares one connection, and a round reads until it has what it was told to read at most,
//! the workload closes the connection, or its wait runs out; over UDP a round sends one datagram
//! and reads the first that comes back. The probe's timeout bounds what the workload is given,
//! connecting and every read together; a question the control plane does not answer within ten
//! seconds leaves the attempt unjudged, since a slow provider is not the workload's failure.

use super::{Queries, Tried};
use korpis_core::extension::health::{MAX_READ, MAX_ROUNDS};
use korpis_proto::korpis::agent::v1::__buffa::oneof::health_answer::Outcome;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::health_next::Next;
use std::net::SocketAddr;
use std::os::fd::OwnedFd;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// The longest the node waits for the control plane to answer one question: as long as the
/// control plane gives the provider.
const ASK_WITHIN: Duration = Duration::from_secs(10);

/// A query probe, as the node runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// The endpoint: the workload's address and the port it declares.
    pub at: SocketAddr,
    /// Whether the endpoint is reached by datagrams.
    pub udp: bool,
    /// The workload, `wl_…`.
    pub workload: String,
    /// The intent its object was built from, `int_…`, whose probe this is.
    pub intent: String,
    /// Whether it is the liveness probe, rather than the readiness one.
    pub liveness: bool,
}

/// The way to the endpoint.
enum Conn {
    Tcp(tokio::net::TcpStream),
    Udp(tokio::net::UdpSocket),
}

impl Conn {
    async fn open(socket: OwnedFd, q: &Query) -> std::io::Result<Self> {
        if q.udp {
            let std = std::net::UdpSocket::from(socket);
            std.set_nonblocking(true)?;
            let udp = tokio::net::UdpSocket::from_std(std)?;
            udp.connect(q.at).await?;
            return Ok(Self::Udp(udp));
        }
        let std = std::net::TcpStream::from(socket);
        std.set_nonblocking(true)?;
        Ok(Self::Tcp(tokio::net::TcpSocket::from_std_stream(std).connect(q.at).await?))
    }

    /// Sends `data`, then reads at most `at_most` bytes within `wait`: the round run.
    async fn round(&mut self, data: &[u8], at_most: usize, wait: Duration) -> pb::HealthRound {
        let mut received = vec![0_u8; at_most];
        let mut got = 0;
        let mut closed = false;
        let until = tokio::time::Instant::now() + wait;
        match self {
            Self::Udp(udp) => {
                if (data.is_empty() || udp.send(data).await.is_ok())
                    && let Ok(Ok(n)) = tokio::time::timeout_at(until, udp.recv(&mut received)).await
                {
                    got = n;
                }
            }
            Self::Tcp(tcp) => {
                closed = !data.is_empty() && tcp.write_all(data).await.is_err();
                while !closed && got < at_most {
                    let rest = received.get_mut(got..).unwrap_or_default();
                    match tokio::time::timeout_at(until, tcp.read(rest)).await {
                        Ok(Ok(0) | Err(_)) => closed = true,
                        Ok(Ok(n)) => got += n,
                        Err(_) => break,
                    }
                }
            }
        }
        received.truncate(got);
        pb::HealthRound { sent: data.to_vec(), received, closed, ..Default::default() }
    }
}

/// Runs `q`'s exchange from `socket`, the workload given `budget` in all; what it found.
pub(super) async fn exchange(
    socket: OwnedFd,
    q: &Query,
    budget: Duration,
    queries: &Queries,
) -> Tried {
    let started = Instant::now();
    let Ok(Ok(mut conn)) = tokio::time::timeout(budget, Conn::open(socket, q)).await else {
        return Tried::Failed;
    };
    let mut left = budget.saturating_sub(started.elapsed());
    let mut rounds: Vec<pb::HealthRound> = Vec::new();
    loop {
        let Some(Outcome::Next(next)) = queries.ask(q, &rounds, ASK_WITHIN).await else {
            return Tried::Unjudged;
        };
        match next.next {
            Some(Next::Verdict(v)) => return if v.healthy { Tried::Passed } else { Tried::Failed },
            Some(Next::Send(s)) if rounds.len() < MAX_ROUNDS => {
                let wait =
                    s.wait.as_option().and_then(|w| korpis_wire::duration_from("wait", w).ok());
                let wait = wait.unwrap_or_default().min(left);
                let at_most = usize::try_from(s.read_at_most.min(MAX_READ)).unwrap_or(0);
                let began = Instant::now();
                rounds.push(conn.round(&s.data, at_most, wait).await);
                left = left.saturating_sub(began.elapsed());
            }
            // A round past the bounds, which the control plane refuses: nothing to judge by.
            _ => return Tried::Unjudged,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use korpis_extension::health::{send, verdict};
    use korpis_proto::korpis::agent::v1 as agent;
    use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
    use std::sync::{Arc, Weak};
    use tokio::net::{TcpListener, UdpSocket};
    use tokio::sync::mpsc;

    fn socket(udp: bool) -> OwnedFd {
        use rustix::net::{AddressFamily, SocketFlags, SocketType};
        let kind = if udp { SocketType::DGRAM } else { SocketType::STREAM };
        rustix::net::socket_with(AddressFamily::INET, kind, SocketFlags::CLOEXEC, None).unwrap()
    }

    /// The echo protocol's provider, answering `q`: send `ping`, and healthy if the round read
    /// `pong`.
    fn echo(q: &agent::HealthQuery) -> agent::HealthAnswer {
        let next = match q.rounds.last() {
            None => send(b"ping", 16, Duration::from_secs(1)),
            Some(r) => verdict(r.received.starts_with(b"pong"), ""),
        };
        let outcome = Some(Outcome::Next(Box::new(next)));
        agent::HealthAnswer { request: q.request.clone(), outcome, ..Default::default() }
    }

    /// Plays the control plane with [`echo`] until the questions end; how many rounds each
    /// question it was asked named. It holds the questions weakly, as the session's channel ends
    /// only once they are dropped.
    async fn control_plane(
        queries: Weak<Queries>,
        mut asked: mpsc::Receiver<agent::AgentMessage>,
    ) -> Vec<usize> {
        let mut rounds = Vec::new();
        while let Some(m) = asked.recv().await {
            let Some(Up::HealthQuery(q)) = m.message else { continue };
            rounds.push(q.rounds.len());
            if let Some(queries) = queries.upgrade() {
                queries.answer(echo(&q));
            }
        }
        rounds
    }

    /// Answers the first four bytes of each connection with `pong` if they were `ping`.
    async fn pong(listener: TcpListener) {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut got = [0_u8; 4];
            let _ = s.read_exact(&mut got).await;
            let _ = s.write_all(if &got == b"ping" { b"pong" } else { b"what" }).await;
        }
    }

    /// Answers every datagram with `nope`.
    async fn nope(udp: UdpSocket) {
        let mut got = [0_u8; 16];
        while let Ok((_, from)) = udp.recv_from(&mut got).await {
            let _ = udp.send_to(b"nope", from).await;
        }
    }

    fn query(at: SocketAddr, udp: bool) -> Query {
        Query { at, udp, workload: "wl_x".into(), intent: "int_x".into(), liveness: true }
    }

    #[tokio::test]
    async fn the_node_runs_the_rounds_the_provider_asks_and_takes_its_verdict() {
        let (tx, rx) = mpsc::channel(8);
        let queries = Queries::new(tx);
        let asked = tokio::spawn(control_plane(Arc::downgrade(&queries), rx));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = listener.local_addr().unwrap();
        tokio::spawn(pong(listener));
        let second = Duration::from_secs(2);
        let tried = exchange(socket(false), &query(at, false), second, &queries).await;
        assert_eq!(tried, Tried::Passed, "pong over TCP");

        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let at = udp.local_addr().unwrap();
        tokio::spawn(nope(udp));
        let tried = exchange(socket(true), &query(at, true), second, &queries).await;
        assert_eq!(tried, Tried::Failed, "a datagram answered, and not with pong");

        drop(queries);
        let rounds = asked.await.unwrap();
        assert_eq!(rounds, [0, 1, 0, 1], "asked first with no round, then with the one run");
    }

    #[tokio::test]
    async fn no_answer_from_the_control_plane_is_no_verdict() {
        // A session that has ended: the question cannot be asked.
        let (tx, rx) = mpsc::channel(8);
        drop(rx);
        let queries = Queries::new(tx);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let at = listener.local_addr().unwrap();
        let q = query(at, false);
        let tried = exchange(socket(false), &q, Duration::from_secs(1), &queries).await;
        assert_eq!(tried, Tried::Unjudged, "the workload is not failed for it");
    }
}
