// SPDX-License-Identifier: AGPL-3.0-or-later

//! A node's side of the mesh (§9.1 of docs/07-networking.md), as its agent reports it, and the
//! peers each node is told of. Read strictly and bounded: a node's pools become routes on every
//! other node, and its key what they accept from.

use crate::{IdKind, Result, invalid, wire_id};
use ipnet::IpNet;
use korpis_core::node::mesh::{Key, Mesh, OverlayFacts, Standing};
use korpis_proto::korpis::agent::v1 as agent;
use std::time::{Duration, UNIX_EPOCH};
use uuid::Uuid;

/// The most pools a node reports, and peers' handshakes.
const MOST_POOLS: usize = 16;
const MOST_HANDSHAKES: usize = 4096;

/// A key of 32 bytes in `field`.
fn key(field: &str, bytes: &[u8]) -> Result<Key> {
    bytes.try_into().map_err(|_| invalid(field, "a WireGuard key, 32 bytes"))
}

/// A node's side of the mesh as its agent reports it in `field`.
///
/// # Errors
/// A key not 32 bytes, no port, a pool that is no network, or a bound broken.
pub fn overlay_from_pb(field: &str, o: &agent::OverlayFacts) -> Result<OverlayFacts> {
    if o.pools.len() > MOST_POOLS || o.handshakes.len() > MOST_HANDSHAKES {
        return Err(invalid(field, "at most 16 pools and 4,096 handshakes"));
    }
    let port = u16::try_from(o.port)
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| invalid(&format!("{field}.port"), "a UDP port"))?;
    let pools = o
        .pools
        .iter()
        .map(|p| p.parse::<IpNet>().map(|n| n.trunc()))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| invalid(&format!("{field}.pools"), "networks, as CIDR text"))?;
    let mut handshakes = std::collections::BTreeMap::new();
    for h in &o.handshakes {
        let at = u64::try_from(h.latest_unix_seconds).ok().filter(|s| *s > 0);
        if let Some(secs) = at {
            let peer = key(&format!("{field}.handshakes"), &h.public_key)?;
            handshakes.insert(peer, UNIX_EPOCH + Duration::from_secs(secs));
        }
    }
    Ok(OverlayFacts {
        key: key(&format!("{field}.public_key"), &o.public_key)?,
        port,
        pools,
        handshakes,
    })
}

/// A node's side of the mesh as the agent reports it.
#[must_use]
pub fn overlay_to_pb(o: &OverlayFacts) -> agent::OverlayFacts {
    let handshakes = o.handshakes.iter().map(|(k, at)| agent::OverlayHandshake {
        public_key: k.to_vec(),
        latest_unix_seconds: at
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|d| i64::try_from(d.as_secs()).ok())
            .unwrap_or(0),
        ..Default::default()
    });
    agent::OverlayFacts {
        public_key: o.key.to_vec(),
        port: u32::from(o.port),
        pools: o.pools.iter().map(ToString::to_string).collect(),
        handshakes: handshakes.collect(),
        ..Default::default()
    }
}

/// What `node` is told of `mesh`: every other member, or none and why it is outside; nothing
/// for a node that reports no mesh interface, which keeps none to tell.
#[must_use]
pub fn peers_to_pb(mesh: &Mesh, node: Uuid) -> Option<agent::Peers> {
    if let Some((_, why)) = mesh.outside.iter().find(|(n, _)| *n == node) {
        return Some(agent::Peers { outside: why.clone(), ..Default::default() });
    }
    if !mesh.members.iter().any(|m| m.node == node) {
        return None;
    }
    let peers = mesh.members.iter().filter(|m| m.node != node).map(|m| agent::Peer {
        node: wire_id(IdKind::Node, m.node),
        public_key: m.key.to_vec(),
        endpoint: m.endpoint.map(|e| e.to_string()).unwrap_or_default(),
        allowed_ips: m.pools.iter().map(ToString::to_string).collect(),
        ..Default::default()
    });
    Some(agent::Peers { peers: peers.collect(), ..Default::default() })
}

/// Sets `node`'s place in the mesh, as the control plane judged it from what every node reports
/// (§9.1 of docs/07-networking.md): available when it is a member reaching every other one, its
/// side as its agent reported it, and each pair with it.
pub(super) fn standing_into(node: &mut korpis_proto::korpis::v1::Node, s: &Standing) {
    use korpis_proto::korpis::v1 as pb;
    node.overlay =
        if s.available { pb::Overlay::OVERLAY_AVAILABLE } else { pb::Overlay::OVERLAY_UNAVAILABLE }
            .into();
    if let Some(f) = &s.facts {
        node.overlay_key = f.key.to_vec();
        node.overlay_port = u32::from(f.port);
        node.pools = f.pools.iter().map(ToString::to_string).collect();
    }
    node.peers = s
        .peers
        .iter()
        .map(|(peer, reachable)| pb::PeerReach {
            node: wire_id(IdKind::Node, *peer),
            reachable: *reachable,
            ..Default::default()
        })
        .collect();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use korpis_core::node::mesh::Member;

    #[test]
    fn a_node_s_side_is_read_strictly_and_written_back_alike() {
        let facts = OverlayFacts {
            key: [3; 32],
            port: 51820,
            pools: vec!["10.48.4.0/22".parse().unwrap()],
            handshakes: [([4; 32], UNIX_EPOCH + Duration::from_secs(1_000))].into(),
        };
        let pb = overlay_to_pb(&facts);
        assert_eq!(overlay_from_pb("overlay", &pb).unwrap(), facts);
        let mut bad = pb.clone();
        bad.public_key.pop();
        assert!(overlay_from_pb("overlay", &bad).is_err());
        let mut bad = pb;
        bad.pools = vec!["10.48.4.0".into()];
        assert!(overlay_from_pb("overlay", &bad).is_err());
    }

    #[test]
    fn a_member_is_told_of_every_other_a_node_outside_why_and_one_without_an_interface_nothing() {
        let member = |n: u128| Member {
            node: Uuid::from_u128(n),
            key: [u8::try_from(n).unwrap(); 32],
            endpoint: None,
            pools: vec![],
        };
        let mesh = Mesh {
            members: vec![member(1), member(2)],
            outside: vec![(Uuid::from_u128(3), "its pool overlaps".into())],
        };
        let told = peers_to_pb(&mesh, Uuid::from_u128(1)).unwrap();
        assert_eq!(told.peers.len(), 1);
        assert_eq!(told.peers[0].public_key, vec![2; 32]);
        let outside = peers_to_pb(&mesh, Uuid::from_u128(3)).unwrap();
        assert_eq!(outside.outside, "its pool overlaps");
        assert!(peers_to_pb(&mesh, Uuid::from_u128(4)).is_none(), "no interface, nothing told");
    }
}
