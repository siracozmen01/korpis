// SPDX-License-Identifier: AGPL-3.0-or-later

//! Edge groups, `korpis.v1.EdgeGroup`, and what a node is told of edges, `korpis.agent.v1.Edges`
//! (§3.1 of docs/07-networking.md, §3.6 of docs/01-model.md).

use crate::spec::endpoints::{protocol_from_pb, protocol_to_pb};
use crate::{Result, duration, id, invalid, timestamp, wire_id};
use korpis_core::edge::EdgeGroup;
use korpis_core::edge::table::{Admit, Forward, Forwarding, NodeEdges, Source, Target};
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::net::{IpAddr, Ipv4Addr};

/// The wire form of a group.
#[must_use]
pub fn group_to_pb(g: &EdgeGroup) -> pb::EdgeGroup {
    pb::EdgeGroup {
        id: wire_id(IdKind::EdgeGroup, g.id),
        organization: wire_id(IdKind::Organization, g.organization),
        name: g.name.clone(),
        members: g.members.iter().map(|m| wire_id(IdKind::Node, *m)).collect(),
        first_port: u32::from(g.ports.0),
        last_port: u32::from(g.ports.1),
        probe_port: u32::from(g.probe_port),
        probe_interval: duration(g.probe_interval).into(),
        probe_misses: g.probe_misses,
        term: duration(g.term).into(),
        created_at: timestamp(g.created_at).into(),
        ..Default::default()
    }
}

fn ip(field: &str, text: &str) -> Result<IpAddr> {
    text.parse().map_err(|_| invalid(field, "an IP address"))
}

fn v4(field: &str, text: &str) -> Result<Option<Ipv4Addr>> {
    if text.is_empty() {
        return Ok(None);
    }
    text.parse().map(Some).map_err(|_| invalid(field, "an IPv4 address"))
}

fn port(field: &str, n: u32) -> Result<u16> {
    u16::try_from(n).map_err(|_| invalid(field, "a port"))
}

fn text(a: Option<Ipv4Addr>) -> String {
    a.map(|a| a.to_string()).unwrap_or_default()
}

fn forward_to_pb(f: &Forward) -> agent::EdgeForward {
    let mut out = agent::EdgeForward {
        address: f.address.to_string(),
        port: u32::from(f.port),
        protocol: protocol_to_pb(f.protocol).into(),
        ..Default::default()
    };
    if let Some((t, mesh)) = &f.target {
        out.node = wire_id(IdKind::Node, t.node);
        out.mesh_address = text(*mesh);
        out.workload_address = t.workload_address.to_string();
        out.container_port = u32::from(t.container_port);
    }
    out
}

fn forwarding_to_pb(f: &Forwarding) -> agent::EdgeForwarding {
    agent::EdgeForwarding {
        group: wire_id(IdKind::EdgeGroup, f.group),
        probe_port: u32::from(f.probe_port),
        probe_node: f.probe.map(|(n, _)| wire_id(IdKind::Node, n)).unwrap_or_default(),
        probe_mesh_address: text(f.probe.map(|(_, a)| a)),
        forwards: f.forwards.iter().map(forward_to_pb).collect(),
        ..Default::default()
    }
}

fn admit_to_pb(a: &Admit) -> agent::EdgeAdmit {
    agent::EdgeAdmit {
        workload_address: a.address.to_string(),
        container_port: u32::from(a.port),
        protocol: protocol_to_pb(a.protocol).into(),
        ..Default::default()
    }
}

/// The wire form of what a node is told of edges.
#[must_use]
pub fn node_edges_to_pb(e: &NodeEdges) -> agent::Edges {
    let source = |s: &Source| agent::EdgeSource {
        node: wire_id(IdKind::Node, s.node),
        mesh_address: s.mesh_address.to_string(),
        admits: s.admits.iter().map(admit_to_pb).collect(),
        ..Default::default()
    };
    agent::Edges {
        mesh_address: text(e.mesh_address),
        groups: e.groups.iter().map(forwarding_to_pb).collect(),
        sources: e.sources.iter().map(source).collect(),
        ..Default::default()
    }
}

fn forward_from_pb(f: &agent::EdgeForward) -> Result<Forward> {
    let target = if f.node.is_empty() {
        None
    } else {
        let t = Target {
            node: id(IdKind::Node, "forward.node", &f.node)?,
            workload_address: ip("forward.workload_address", &f.workload_address)?,
            container_port: port("forward.container_port", f.container_port)?,
        };
        Some((t, v4("forward.mesh_address", &f.mesh_address)?))
    };
    Ok(Forward {
        address: ip("forward.address", &f.address)?,
        port: port("forward.port", f.port)?,
        protocol: protocol_from_pb("forward.protocol", f.protocol)?,
        target,
    })
}

fn forwarding_from_pb(f: &agent::EdgeForwarding) -> Result<Forwarding> {
    let probe = match v4("group.probe_mesh_address", &f.probe_mesh_address)? {
        Some(a) => Some((id(IdKind::Node, "group.probe_node", &f.probe_node)?, a)),
        None => None,
    };
    Ok(Forwarding {
        group: id(IdKind::EdgeGroup, "group.group", &f.group)?,
        probe_port: port("group.probe_port", f.probe_port)?,
        probe,
        forwards: f.forwards.iter().map(forward_from_pb).collect::<Result<_>>()?,
    })
}

fn source_from_pb(s: &agent::EdgeSource) -> Result<Source> {
    let admit = |a: &agent::EdgeAdmit| -> Result<Admit> {
        Ok(Admit {
            address: ip("admit.workload_address", &a.workload_address)?,
            port: port("admit.container_port", a.container_port)?,
            protocol: protocol_from_pb("admit.protocol", a.protocol)?,
        })
    };
    let mesh = v4("source.mesh_address", &s.mesh_address)?;
    Ok(Source {
        node: id(IdKind::Node, "source.node", &s.node)?,
        mesh_address: mesh.ok_or_else(|| invalid("source.mesh_address", "an IPv4 address"))?,
        admits: s.admits.iter().map(admit).collect::<Result<_>>()?,
    })
}

/// What a node is told of edges, from the wire.
///
/// # Errors
/// A field that is not what it says.
pub fn node_edges_from_pb(e: &agent::Edges) -> Result<NodeEdges> {
    Ok(NodeEdges {
        mesh_address: v4("edges.mesh_address", &e.mesh_address)?,
        groups: e.groups.iter().map(forwarding_from_pb).collect::<Result<_>>()?,
        sources: e.sources.iter().map(source_from_pb).collect::<Result<_>>()?,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use korpis_core::workload::spec::endpoints::Protocol;
    use uuid::Uuid;

    #[test]
    fn what_a_node_is_told_of_edges_crosses_the_wire_unchanged() {
        let target = Target {
            node: Uuid::from_u128(3),
            workload_address: "10.48.4.2".parse().unwrap(),
            container_port: 25_565,
        };
        let forward = |target| Forward {
            address: "198.51.100.2".parse().unwrap(),
            port: 25_565,
            protocol: Protocol::Udp,
            target,
        };
        let told = NodeEdges {
            mesh_address: "10.48.0.255".parse().ok(),
            groups: vec![Forwarding {
                group: Uuid::from_u128(10),
                probe_port: 65_535,
                probe: Some((Uuid::from_u128(3), "10.48.7.255".parse().unwrap())),
                forwards: vec![forward(Some((target, "10.48.7.255".parse().ok()))), forward(None)],
            }],
            sources: vec![Source {
                node: Uuid::from_u128(2),
                mesh_address: "10.48.11.255".parse().unwrap(),
                admits: vec![Admit {
                    address: "10.48.0.2".parse().unwrap(),
                    port: 80,
                    protocol: Protocol::Tcp,
                }],
            }],
        };
        assert_eq!(node_edges_from_pb(&node_edges_to_pb(&told)).unwrap(), told);
    }
}
