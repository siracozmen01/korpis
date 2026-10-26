// SPDX-License-Identifier: AGPL-3.0-or-later

//! The reachability prober of this process (§9 of docs/07-networking.md): for every node whose
//! session it holds and whose advertised addresses are unmeasured or measured too long ago, it
//! asks the node to listen on a port of its range, then tries each advertised address over TCP
//! and over UDP, sending a nonce and checking that the answer is the node key's signature. The
//! node is `public` if one address answers over both, and `edge_only` otherwise. The result is
//! recorded only if the node still advertises what was measured.
//!
//! The measurement is from where this process runs. A prober inside the node's network, or on
//! the node's own machine, reaches what a client on the internet may not; §9 says what that
//! costs.

use crate::api::Api;
use korpis_app::node::ProbeDue;
use korpis_core::id::IdKind;
use korpis_core::node::Reachability;
use korpis_core::node::identity::{probe_answer_message, verify};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_wire::wire_id;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpStream, UdpSocket};
use tokio::time::Instant;
use uuid::Uuid;

/// How long a measurement stands before the node is measured again.
pub const EVERY: Duration = Duration::from_secs(600);
/// How often nodes are looked at.
const TICK: Duration = Duration::from_secs(2);
/// How long after an attempt that measured nothing a node is tried again.
pub(crate) const RETRY: Duration = Duration::from_secs(30);
/// How long a node has to start listening.
const LISTEN: Duration = Duration::from_secs(10);
/// How long one connection or one answer may take.
const ANSWER: Duration = Duration::from_secs(3);
/// How many datagrams are sent before UDP is judged unreachable.
const DATAGRAMS: usize = 3;
/// How many ports a node is asked to listen on, one after another, before an attempt fails.
const PORTS: u32 = 3;

/// Where a node's last attempt stands.
#[derive(Debug, Clone, Copy)]
enum Attempt {
    /// Being probed now.
    InFlight,
    /// The last attempt measured nothing, at this instant.
    Failed(Instant),
}

/// The nodes being probed, or tried recently without a measurement.
type Attempts = Arc<Mutex<HashMap<Uuid, Attempt>>>;

/// `N` random bytes, or nothing when the system has none to give.
pub(crate) fn random<const N: usize>() -> Option<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes)
}

/// `bytes` in lowercase hexadecimal.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Whether `answer` is `node`'s signature for `nonce` on `port`.
fn signed(due: &ProbeDue, node: &str, port: u16, nonce: &[u8], answer: &[u8]) -> bool {
    verify(&due.key, &probe_answer_message(node, port, nonce), answer)
}

async fn over_tcp(at: SocketAddr, nonce: &[u8; 32]) -> Option<Vec<u8>> {
    let mut stream = tokio::time::timeout(ANSWER, TcpStream::connect(at)).await.ok()?.ok()?;
    stream.write_all(nonce).await.ok()?;
    let mut answer = vec![0u8; 64];
    tokio::time::timeout(ANSWER, stream.read_exact(&mut answer)).await.ok()?.ok()?;
    Some(answer)
}

async fn over_udp(at: SocketAddr, nonce: &[u8; 32]) -> Option<Vec<u8>> {
    let any: SocketAddr = if at.is_ipv4() { ([0u8; 4], 0).into() } else { ([0u16; 8], 0).into() };
    let socket = UdpSocket::bind(any).await.ok()?;
    let mut answer = vec![0u8; 64];
    for _ in 0..DATAGRAMS {
        socket.send_to(nonce, at).await.ok()?;
        let received = tokio::time::timeout(ANSWER, socket.recv_from(&mut answer)).await;
        if let Ok(Ok((64, from))) = received
            && from == at
        {
            return Some(answer);
        }
    }
    None
}

/// Whether `address` answers as the node on `port` over TCP and over UDP.
async fn answers(due: &ProbeDue, node: &str, address: IpAddr, port: u16) -> bool {
    let at = SocketAddr::new(address, port);
    let (Some(tcp), Some(udp)) = (random::<32>(), random::<32>()) else { return false };
    let by_tcp = over_tcp(at, &tcp).await.is_some_and(|a| signed(due, node, port, &tcp, &a));
    by_tcp && over_udp(at, &udp).await.is_some_and(|a| signed(due, node, port, &udp, &a))
}

/// Asks `node` to listen on a free port of its range, the next free one each time it says it
/// cannot, up to [`PORTS`] of them; the port it listens on.
async fn listen(api: &Api, node: Uuid, due: &ProbeDue) -> Result<u16, String> {
    let first = random::<4>().map(u32::from_le_bytes).ok_or("no randomness")?;
    let mut why = String::new();
    for next in 0..PORTS {
        let pick = first.wrapping_add(next);
        let port = due.free_port(pick).ok_or("every port of the node's range is held")?;
        let probe = hex(&random::<16>().ok_or("no randomness")?);
        let sender = api.sessions().sender(node).ok_or("the node's session ended")?;
        let answered = api.sessions().expect_probe(&probe);
        let ask = agent::ReachabilityProbe {
            probe: probe.clone(),
            port: u32::from(port),
            ..Default::default()
        };
        let message = agent::ControlMessage {
            message: Some(Down::ReachabilityProbe(Box::new(ask))),
            ..Default::default()
        };
        if sender.send(message).await.is_err() {
            api.sessions().forget_probe(&probe);
            return Err("the node's session ended".into());
        }
        let Ok(Ok(error)) = tokio::time::timeout(LISTEN, answered).await else {
            api.sessions().forget_probe(&probe);
            return Err("the node did not say it was listening".into());
        };
        if error.is_empty() {
            return Ok(port);
        }
        // A port taken on the node's machine, or forwarded to a workload, says nothing of the
        // node's reachability: another port is asked for at once.
        why = error;
    }
    Err(why)
}

/// One measurement of `node`.
async fn measure(api: &Api, node: Uuid, due: &ProbeDue) -> Result<Reachability, String> {
    let port = listen(api, node, due).await?;
    let name = wire_id(IdKind::Node, node);
    for address in &due.advertised.addresses {
        if answers(due, &name, *address, port).await {
            return Ok(Reachability::Public);
        }
    }
    Ok(Reachability::EdgeOnly)
}

/// Measures `node` and records it, then lets it be tried again.
async fn probe(api: Arc<Api>, node: Uuid, due: ProbeDue, attempts: Attempts) {
    match measure(&api, node, &due).await {
        Ok(reachability) => {
            match api.app().record_reachability(node, &due.advertised, reachability).await {
                Ok(true) => tracing::info!(%node, ?reachability, "reachability measured"),
                Ok(false) => tracing::debug!(%node, "the node's addresses changed while measured"),
                Err(e) => tracing::warn!(%node, error = %e, "recording reachability failed"),
            }
            attempts.lock().unwrap_or_else(PoisonError::into_inner).remove(&node);
        }
        Err(e) => {
            tracing::warn!(%node, error = %e, "reachability not measured");
            let failed = Attempt::Failed(Instant::now());
            attempts.lock().unwrap_or_else(PoisonError::into_inner).insert(node, failed);
        }
    }
}

/// Whether `node` may be looked at now, recording that it is; not while it is being probed, nor
/// for RETRY after an attempt that measured nothing.
fn begin(attempts: &Attempts, node: Uuid) -> bool {
    let mut tried = attempts.lock().unwrap_or_else(PoisonError::into_inner);
    let busy = match tried.get(&node) {
        Some(Attempt::InFlight) => true,
        Some(Attempt::Failed(at)) => at.elapsed() < RETRY,
        None => false,
    };
    if !busy {
        tried.insert(node, Attempt::InFlight);
    }
    !busy
}

/// Looks for nodes due a measurement every TICK, for as long as the process runs.
pub(crate) async fn keep(api: Arc<Api>) {
    let attempts: Attempts = Arc::default();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        for node in api.sessions().nodes() {
            if !begin(&attempts, node) {
                continue;
            }
            match api.app().probe_due(node, EVERY).await {
                Ok(Some(due)) => {
                    tokio::spawn(probe(Arc::clone(&api), node, due, Arc::clone(&attempts)));
                }
                Ok(None) => {
                    attempts.lock().unwrap_or_else(PoisonError::into_inner).remove(&node);
                }
                Err(e) => {
                    tracing::warn!(%node, error = %e, "reachability check failed");
                    let failed = Attempt::Failed(Instant::now());
                    attempts.lock().unwrap_or_else(PoisonError::into_inner).insert(node, failed);
                }
            }
        }
    }
}
