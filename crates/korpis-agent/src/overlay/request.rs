// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the overlay asks of the helper, and what it reports of the answer: the peers, what the
//! mesh admits, the node's part in edge groups, and the nodes admitted to send it replicas (§2.2
//! of docs/06-storage.md).

use super::leases::Holding;
use super::{Overlay, edges};
use crate::held::HeldSet;
use crate::host::Host;
use crate::replicas::{TRANSFER_PORT, mesh_of};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;

/// What the node admits from the mesh, by what `held` names: each dependent on another node, to
/// the endpoint of the workload here it depends on.
pub(super) fn admitted(held: &HeldSet) -> Vec<helper::OverlayAdmitted> {
    let mut out: Vec<helper::OverlayAdmitted> = held
        .assignments
        .values()
        .flat_map(|a| &a.dependents)
        .map(|d| helper::OverlayAdmitted {
            source: d.address.clone(),
            destination: d.target_address.clone(),
            port: d.container_port,
            transport: match d.protocol.as_known() {
                Some(
                    pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP
                    | pb::EndpointProtocol::ENDPOINT_PROTOCOL_QUIC,
                ) => helper::Transport::TRANSPORT_UDP,
                _ => helper::Transport::TRANSPORT_TCP,
            }
            .into(),
            ..Default::default()
        })
        .collect();
    out.sort_by(|x, y| {
        (&x.source, &x.destination, x.port).cmp(&(&y.source, &y.destination, y.port))
    });
    out.dedup();
    out
}

/// The request that keeps the interface to `peers` on `port`, admitting `admitted` and what the
/// edges' proxies may reach here, with the node's part in edge groups as `edges` tells it (§3.1
/// of docs/07-networking.md), holding the addresses of `holding`, the node advertising `own`.
pub(super) fn request(
    (port, own): (u16, &[String]),
    (peers, edges, holding): (&agent::Peers, &agent::Edges, &Holding),
    mut admitted: Vec<helper::OverlayAdmitted>,
) -> helper::KeepOverlayRequest {
    admitted.extend(edges::proxied(edges));
    admitted.sort_by(|x, y| {
        (&x.source, &x.destination, x.port).cmp(&(&y.source, &y.destination, y.port))
    });
    admitted.dedup();
    let peers = peers.peers.iter().map(|p| helper::OverlayPeer {
        public_key: p.public_key.clone(),
        endpoint: p.endpoint.clone(),
        allowed_ips: p.allowed_ips.clone(),
        ..Default::default()
    });
    helper::KeepOverlayRequest {
        port: u32::from(port),
        peers: peers.collect(),
        admitted,
        mesh_address: edges.mesh_address.clone(),
        mesh_address_v6: edges.mesh_address_v6.clone(),
        edges: edges::table(edges, own, holding).into(),
        ..Default::default()
    }
}

/// What the node reports of what the helper answered.
pub(super) fn facts(
    port: u16,
    pools: &[String],
    kept: helper::KeepOverlayResponse,
) -> agent::OverlayFacts {
    let handshakes = kept.handshakes.into_iter().map(|h| agent::OverlayHandshake {
        public_key: h.public_key,
        latest_unix_seconds: h.latest_unix_seconds,
        ..Default::default()
    });
    agent::OverlayFacts {
        public_key: kept.public_key,
        port: u32::from(port),
        pools: pools.to_vec(),
        handshakes: handshakes.collect(),
        ..Default::default()
    }
}

impl<H: Host> Overlay<H> {
    /// Admits to `asked`'s transfer port the mesh address of each node, among `named`, that what
    /// the node was last told of replicas names as sending to it; and puts the node's own mesh
    /// address on the interface even where no edge group names it, since it receives there.
    pub(super) fn transfers(&self, named: &agent::Peers, asked: &mut helper::KeepOverlayRequest) {
        if asked.mesh_address.is_empty() {
            let pools: Vec<ipnet::IpNet> =
                self.pools.iter().filter_map(|p| p.parse().ok()).collect();
            if let Some(own) = korpis_core::edge::mesh_address(&pools) {
                asked.mesh_address = own.to_string();
            }
        }
        let read = std::fs::read(&self.replicas).ok();
        let told: agent::Replicas =
            read.and_then(|b| buffa::Message::decode_from_slice(&b).ok()).unwrap_or_default();
        let mut sources: Vec<String> = told
            .keeping
            .iter()
            .filter_map(|k| mesh_of(named, &k.from).map(|a| a.to_string()))
            .collect();
        sources.sort();
        sources.dedup();
        asked.transfer_sources = sources;
        asked.transfer_port = u32::from(TRANSFER_PORT);
    }
}
