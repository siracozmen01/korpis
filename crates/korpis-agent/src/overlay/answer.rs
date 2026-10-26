// SPDX-License-Identifier: AGPL-3.0-or-later

//! Answering the control plane's probe of the edges that forward to this node (§3.1 of
//! docs/07-networking.md). The node listens on its own addresses in the mesh, on each probe port
//! an edge forwards to it, and answers each probe with its node key's signature over the probe's
//! group, the address it probed, the node, and the nonce. The probe reaches that address only
//! through an edge's GRE link and the edge's translation, and the answer leaves through them, so
//! an answer the control plane verifies proves the edge's forwarding both ways.

use ed25519_dalek::SigningKey;
use korpis_core::edge::probe::{REQUEST, answer_message, read_request};
use korpis_core::node::identity::sign;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

/// The node answering, by wire identifier, and its key.
pub(super) type Signer = (String, SigningKey);

/// Where `told` has the node answer probes: its mesh addresses, IPv4 and IPv6, on each probe
/// port an edge's admits name for that address over UDP.
fn wanted(told: &agent::Edges) -> BTreeSet<SocketAddr> {
    let mesh: Vec<IpAddr> = [&told.mesh_address, &told.mesh_address_v6]
        .into_iter()
        .filter_map(|a| a.parse().ok())
        .collect();
    let udp = |a: &agent::EdgeAdmit| {
        a.protocol.as_known() == Some(pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP)
    };
    told.sources
        .iter()
        .flat_map(|s| &s.admits)
        .filter(|a| udp(a))
        .filter_map(|a| {
            let at = a.workload_address.parse::<IpAddr>().ok().filter(|at| mesh.contains(at))?;
            Some(SocketAddr::new(at, u16::try_from(a.container_port).ok()?))
        })
        .collect()
}

/// Answers each probe arriving on `socket` until it fails.
async fn serve(socket: UdpSocket, signer: Arc<Signer>) {
    let mut buffer = [0_u8; REQUEST + 1];
    while let Ok((n, from)) = socket.recv_from(&mut buffer).await {
        if let Some((nonce, group, address)) = buffer.get(..n).and_then(read_request) {
            let message = answer_message(group, address, &signer.0, &nonce);
            let _ = socket.send_to(&sign(&signer.1, &message), from).await;
        }
    }
}

/// The node's answering, one task per address and port it listens on.
#[derive(Debug)]
pub(super) struct Answering {
    signer: Arc<Signer>,
    listening: BTreeMap<SocketAddr, JoinHandle<()>>,
}

impl Answering {
    /// Answering nothing yet, as `signer`.
    pub(super) fn new(signer: Signer) -> Self {
        Self { signer: Arc::new(signer), listening: BTreeMap::new() }
    }

    /// Listens on exactly where `told` has the node answer; called once the helper has put the
    /// node's mesh address in place. One it cannot listen on is tried again at the next call.
    pub(super) async fn keep(&mut self, told: &agent::Edges) {
        let wanted = wanted(told);
        self.listening.retain(|at, task| {
            let kept = wanted.contains(at) && !task.is_finished();
            if !kept {
                task.abort();
            }
            kept
        });
        for at in wanted {
            if self.listening.contains_key(&at) {
                continue;
            }
            match UdpSocket::bind(at).await {
                Ok(socket) => {
                    let task = tokio::spawn(serve(socket, Arc::clone(&self.signer)));
                    self.listening.insert(at, task);
                }
                Err(e) => tracing::warn!(%at, error = %e, "edge probes cannot be answered"),
            }
        }
    }
}

impl Drop for Answering {
    fn drop(&mut self) {
        for task in self.listening.values() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use korpis_core::edge::probe::{NONCE, request};
    use korpis_core::node::identity::verify;
    use uuid::Uuid;

    #[tokio::test]
    async fn a_probe_on_either_mesh_address_is_answered_with_the_node_key_s_signature() {
        let free = |at: &str| std::net::UdpSocket::bind(at).unwrap().local_addr().unwrap().port();
        let (port, port6) = (free("127.0.0.1:0"), free("[::1]:0"));
        let admit = |address: &str, port: u16, protocol: pb::EndpointProtocol| agent::EdgeAdmit {
            workload_address: address.into(),
            container_port: u32::from(port),
            protocol: protocol.into(),
            ..Default::default()
        };
        let told = agent::Edges {
            mesh_address: "127.0.0.1".into(),
            mesh_address_v6: "::1".into(),
            sources: vec![agent::EdgeSource {
                admits: vec![
                    admit("127.0.0.1", port, pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP),
                    admit("::1", port6, pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP),
                    admit("127.0.0.1", port, pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP),
                    admit("10.48.4.2", 25_565, pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(wanted(&told).len(), 2, "the node's own addresses, over UDP, alone");
        let key = SigningKey::from_bytes(&[7; 32]);
        let mut answering = Answering::new(("nd_a".into(), key.clone()));
        answering.keep(&told).await;

        for (from, to, probed) in [
            ("127.0.0.1:0", SocketAddr::from(([127, 0, 0, 1], port)), "198.51.100.2"),
            ("[::1]:0", SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port6)), "2001:db8::2"),
        ] {
            let prober = UdpSocket::bind(from).await.unwrap();
            let (group, address) = (Uuid::from_u128(5), probed.parse().unwrap());
            let nonce = [3; NONCE];
            prober.send_to(&request(&nonce, group, address), to).await.unwrap();
            let mut answer = [0; 64];
            let (n, _) = prober.recv_from(&mut answer).await.unwrap();
            let signed = answer_message(group, address, "nd_a", &nonce);
            assert!(n == 64 && verify(&key.verifying_key().to_bytes(), &signed, &answer));
        }
        answering.keep(&agent::Edges::default()).await;
        assert!(answering.listening.is_empty(), "no edge forwards a probe here any more");
    }
}
