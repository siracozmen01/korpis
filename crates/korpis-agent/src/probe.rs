// SPDX-License-Identifier: AGPL-3.0-or-later

//! Answering a reachability probe (§9 of docs/07-networking.md). A node cannot know from the
//! inside whether it can be reached, so when the control plane asks, it listens on the port it
//! was given, over TCP and over UDP on every address, for a short window, and answers each nonce
//! sent there with its node key's signature. The prober reaching that port at an advertised
//! address and getting the node's signature back is the measurement; a machine that answers
//! without the key is not the node, whatever address it holds.

use ed25519_dalek::SigningKey;
use korpis_core::node::identity::{probe_answer_message, sign};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::time::Instant;

/// How long the node listens for one probe.
pub const WINDOW: Duration = Duration::from_secs(15);
/// The length of a nonce.
const NONCE: usize = 32;

/// What the node answers with.
struct Answerer {
    node: String,
    key: SigningKey,
    port: u16,
}

impl Answerer {
    fn answer(&self, nonce: &[u8]) -> Vec<u8> {
        sign(&self.key, &probe_answer_message(&self.node, self.port, nonce))
    }
}

async fn bind_both(at: SocketAddr) -> std::io::Result<(TcpListener, UdpSocket)> {
    Ok((TcpListener::bind(at).await?, UdpSocket::bind(at).await?))
}

/// Sockets on `port` covering every address: the IPv6 wildcard, which on a dual-stack machine
/// covers IPv4 too, and the IPv4 wildcard where it does not. A port already taken is an error; a
/// family the machine lacks is not.
async fn bind(port: u16) -> std::io::Result<(Vec<TcpListener>, Vec<UdpSocket>)> {
    let (mut tcp, mut udp) = (Vec::new(), Vec::new());
    for at in [SocketAddr::from(([0u16; 8], port)), SocketAddr::from(([0u8; 4], port))] {
        match bind_both(at).await {
            Ok((t, u)) => {
                tcp.push(t);
                udp.push(u);
            }
            // The IPv6 wildcard took IPv4 too.
            Err(e) if e.kind() == ErrorKind::AddrInUse && !tcp.is_empty() => {}
            Err(e) if e.kind() == ErrorKind::AddrInUse => return Err(e),
            // No IPv6 on this machine.
            Err(_) if at.is_ipv6() => {}
            Err(e) => return Err(e),
        }
    }
    Ok((tcp, udp))
}

async fn answer_tcp(mut stream: TcpStream, answerer: Arc<Answerer>) {
    let mut nonce = [0u8; NONCE];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut nonce)).await;
    if matches!(read, Ok(Ok(_))) {
        let _ = stream.write_all(&answerer.answer(&nonce)).await;
    }
}

async fn serve_tcp(listener: TcpListener, answerer: Arc<Answerer>, until: Instant) {
    while let Ok(Ok((stream, _))) = tokio::time::timeout_at(until, listener.accept()).await {
        tokio::spawn(answer_tcp(stream, Arc::clone(&answerer)));
    }
}

async fn serve_udp(socket: UdpSocket, answerer: Arc<Answerer>, until: Instant) {
    let mut buf = [0u8; 64];
    while let Ok(Ok((n, from))) = tokio::time::timeout_at(until, socket.recv_from(&mut buf)).await {
        if let Some(nonce) = buf.get(..n).filter(|b| b.len() == NONCE) {
            let _ = socket.send_to(&answerer.answer(nonce), from).await;
        }
    }
}

fn listening(probe: String, error: String) -> agent::AgentMessage {
    let m = agent::ProbeListening { probe, error, ..Default::default() };
    agent::AgentMessage { message: Some(Up::ProbeListening(Box::new(m))), ..Default::default() }
}

/// Starts answering `probe` as `node` with `key`; what to tell the control plane, which is
/// whether the node is listening. A port in `forwarded` is refused: the node forwards it to a
/// workload, so the prober's nonces would reach the workload and not the node (Finding 53 of
/// docs/23-walkthroughs.md).
pub async fn begin(
    (node, key): (&str, &SigningKey),
    probe: agent::ReachabilityProbe,
    forwarded: &[u16],
) -> agent::AgentMessage {
    let Ok(port) = u16::try_from(probe.port) else {
        return listening(probe.probe, format!("port {} is out of range", probe.port));
    };
    if forwarded.contains(&port) {
        return listening(probe.probe, format!("port {port} is forwarded to a workload"));
    }
    let (tcp, udp) = match bind(port).await {
        Ok(sockets) => sockets,
        Err(e) => return listening(probe.probe, format!("port {port}: {e}")),
    };
    let answerer = Arc::new(Answerer { node: node.to_owned(), key: key.clone(), port });
    let until = Instant::now() + WINDOW;
    for listener in tcp {
        tokio::spawn(serve_tcp(listener, Arc::clone(&answerer), until));
    }
    for socket in udp {
        tokio::spawn(serve_udp(socket, Arc::clone(&answerer), until));
    }
    listening(probe.probe, String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_port_forwarded_to_a_workload_is_not_listened_on() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let probe =
            agent::ReachabilityProbe { probe: "p".into(), port: 30005, ..Default::default() };
        let said = begin(("nd_1", &key), probe, &[30005]).await;
        let Some(Up::ProbeListening(l)) = said.message else { panic!("{said:?}") };
        assert!(l.error.contains("forwarded"), "{}", l.error);
    }
}
