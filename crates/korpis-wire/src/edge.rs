// SPDX-License-Identifier: AGPL-3.0-or-later

//! Edge groups, `korpis.v1.EdgeGroup`, and what a node is told of edges, `korpis.agent.v1.Edges`
//! (§3.1 of docs/07-networking.md, §3.6 of docs/01-model.md).

use crate::assignment::{alpn_challenge_to_pb, http_challenge_to_pb};
use crate::endpoint::{endpoint_to_pb, ingress_endpoint_from_pb};
use crate::spec::endpoints::{protocol_from_pb, protocol_to_pb};
use crate::{Result, duration, id, invalid, timestamp, wire_id};
use korpis_core::edge::EdgeGroup;
use korpis_core::edge::lease::{Health, MemberState};
use korpis_core::edge::probe::AddressStatus;
use korpis_core::edge::table::{
    Admit, Forward, Forwarding, Mesh, NodeEdges, Proxied, Source, Target,
};
use korpis_core::id::IdKind;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::SystemTime;

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

/// `g` with its status: each of `members` as the control plane knows it, and each of `addresses`
/// with its holder at `now`.
#[must_use]
pub fn with_status(
    mut g: pb::EdgeGroup,
    (members, addresses): (&[MemberState], &[AddressStatus]),
    now: SystemTime,
) -> pb::EdgeGroup {
    g.member_status = members
        .iter()
        .map(|m| pb::EdgeMemberStatus {
            node: wire_id(IdKind::Node, m.node),
            ready: m.health() == Health::Ready,
            last_probe: m.probed_at.map(timestamp).into(),
            reason: match m.health() {
                Health::Ready => String::new(),
                Health::Disconnected => "its session is down".into(),
                Health::Unknown => "not yet probed".into(),
                Health::Failing => m.reason.clone().unwrap_or_default(),
            },
            ..Default::default()
        })
        .collect();
    g.holdings = addresses
        .iter()
        .map(|a| pb::EdgeHolding {
            address: a.hold.address.to_string(),
            node: a.hold.holder(now).map(|n| wire_id(IdKind::Node, n)).unwrap_or_default(),
            epoch: a.hold.epoch,
            answering: a.answering == Some(true),
            last_probe: a.probed_at.map(timestamp).into(),
            router: a.route.as_ref().map(|r| r.router.clone()).unwrap_or_default(),
            routed_to: a
                .route
                .as_ref()
                .and_then(|r| r.routed)
                .map(|n| wire_id(IdKind::Node, n))
                .unwrap_or_default(),
            route_failure: a.route.as_ref().and_then(|r| r.failing.clone()).unwrap_or_default(),
            ..Default::default()
        })
        .collect();
    g
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

fn v6(field: &str, text: &str) -> Result<Option<Ipv6Addr>> {
    if text.is_empty() {
        return Ok(None);
    }
    text.parse().map(Some).map_err(|_| invalid(field, "an IPv6 address"))
}

/// A node's addresses in the mesh, from their two fields.
fn mesh(fields: (&str, &str), (v4_text, v6_text): (&str, &str)) -> Result<Option<Mesh>> {
    let Some(at) = v4(fields.0, v4_text)? else { return Ok(None) };
    Ok(Some(Mesh { v4: at, v6: v6(fields.1, v6_text)? }))
}

fn port(field: &str, n: u32) -> Result<u16> {
    u16::try_from(n).map_err(|_| invalid(field, "a port"))
}

fn text(a: Option<impl ToString>) -> String {
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

fn proxied_to_pb(p: &Proxied) -> agent::EdgeProxied {
    agent::EdgeProxied {
        endpoint: endpoint_to_pb(&p.endpoint, &[]).into(),
        node: text(p.target.as_ref().map(|t| wire_id(IdKind::Node, t.node))),
        workload_address: text(p.target.as_ref().map(|t| t.workload_address)),
        ..Default::default()
    }
}

fn forwarding_to_pb(f: &Forwarding) -> agent::EdgeForwarding {
    agent::EdgeForwarding {
        group: wire_id(IdKind::EdgeGroup, f.group),
        probe_port: u32::from(f.probe_port),
        probe_node: f.probe.map(|(n, _)| wire_id(IdKind::Node, n)).unwrap_or_default(),
        probe_mesh_address: text(f.probe.map(|(_, a)| a.v4)),
        probe_mesh_address_v6: text(f.probe.and_then(|(_, a)| a.v6)),
        forwards: f.forwards.iter().map(forward_to_pb).collect(),
        hold: f
            .hold
            .iter()
            .map(|(address, interface)| agent::EdgeAddress {
                address: address.to_string(),
                interface: interface.clone(),
                ..Default::default()
            })
            .collect(),
        ingress: f.ingress.iter().map(ToString::to_string).collect(),
        proxied: f.proxied.iter().map(proxied_to_pb).collect(),
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
        proxied: s.proxied.iter().map(admit_to_pb).collect(),
        ..Default::default()
    };
    agent::Edges {
        mesh_address: text(e.mesh.map(|m| m.v4)),
        mesh_address_v6: text(e.mesh.and_then(|m| m.v6)),
        groups: e.groups.iter().map(forwarding_to_pb).collect(),
        sources: e.sources.iter().map(source).collect(),
        challenges: e.challenges.iter().map(http_challenge_to_pb).collect(),
        alpn_challenges: e.alpn_challenges.iter().map(alpn_challenge_to_pb).collect(),
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

fn proxied_from_pb(group: uuid::Uuid, p: &agent::EdgeProxied) -> Result<Proxied> {
    let missing = || invalid("proxied.endpoint", "an endpoint");
    let endpoint =
        ingress_endpoint_from_pb("proxied.endpoint", p.endpoint.as_option().ok_or_else(missing)?)?;
    let target = if p.node.is_empty() {
        None
    } else {
        Some(Target {
            node: id(IdKind::Node, "proxied.node", &p.node)?,
            workload_address: ip("proxied.workload_address", &p.workload_address)?,
            container_port: endpoint.container_port,
        })
    };
    Ok(Proxied { group, endpoint, target })
}

fn forwarding_from_pb(f: &agent::EdgeForwarding) -> Result<Forwarding> {
    let fields = ("group.probe_mesh_address", "group.probe_mesh_address_v6");
    let probe = match mesh(fields, (&f.probe_mesh_address, &f.probe_mesh_address_v6))? {
        Some(a) => Some((id(IdKind::Node, "group.probe_node", &f.probe_node)?, a)),
        None => None,
    };
    let group = id(IdKind::EdgeGroup, "group.group", &f.group)?;
    Ok(Forwarding {
        group,
        probe_port: port("group.probe_port", f.probe_port)?,
        probe,
        forwards: f.forwards.iter().map(forward_from_pb).collect::<Result<_>>()?,
        hold: f
            .hold
            .iter()
            .map(|h| Ok((ip("group.hold.address", &h.address)?, h.interface.clone())))
            .collect::<Result<_>>()?,
        ingress: f.ingress.iter().map(|a| ip("group.ingress", a)).collect::<Result<_>>()?,
        proxied: f.proxied.iter().map(|p| proxied_from_pb(group, p)).collect::<Result<_>>()?,
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
        proxied: s.proxied.iter().map(admit).collect::<Result<_>>()?,
    })
}

/// What a node is told of edges, from the wire.
///
/// # Errors
/// A field that is not what it says.
pub fn node_edges_from_pb(e: &agent::Edges) -> Result<NodeEdges> {
    Ok(NodeEdges {
        mesh: mesh(
            ("edges.mesh_address", "edges.mesh_address_v6"),
            (&e.mesh_address, &e.mesh_address_v6),
        )?,
        groups: e.groups.iter().map(forwarding_from_pb).collect::<Result<_>>()?,
        sources: e.sources.iter().map(source_from_pb).collect::<Result<_>>()?,
        challenges: e
            .challenges
            .iter()
            .map(|c| korpis_core::dns::certificate::HttpChallenge {
                token: c.token.clone(),
                key_authorization: c.key_authorization.clone(),
            })
            .collect(),
        alpn_challenges: e
            .alpn_challenges
            .iter()
            .map(|c| korpis_core::dns::certificate::AlpnChallenge {
                name: c.name.clone(),
                digest: c.digest.clone(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use korpis_core::dns::certificate::{AlpnChallenge, HttpChallenge};
    use korpis_core::workload::endpoint::{Endpoint, Served};
    use korpis_core::workload::spec::endpoints::{Exposure, Protocol, TlsSpec};
    use uuid::Uuid;

    /// An `ingress` endpoint on node 3, its workload at `target`, its TLS passed through with
    /// a PROXY header.
    fn proxied(target: Option<Target>) -> Proxied {
        let endpoint = Endpoint {
            id: Uuid::from_u128(20),
            workload: Uuid::from_u128(21),
            name: "web".into(),
            protocol: Protocol::Https,
            container_port: 25_565,
            exposure: Exposure::Ingress,
            node: Uuid::from_u128(3),
            port: None,
            domains: vec!["shop.example.com".into()],
            tls: Some(TlsSpec::Passthrough { proxy_protocol: true }),
            certificate: Some(Served { id: Uuid::from_u128(22), revision: 4 }),
        };
        Proxied { group: Uuid::from_u128(10), endpoint, target }
    }

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
            mesh: Some(Mesh { v4: "10.48.0.255".parse().unwrap(), v6: None }),
            groups: vec![Forwarding {
                group: Uuid::from_u128(10),
                probe_port: 65_535,
                probe: Some((
                    Uuid::from_u128(3),
                    Mesh {
                        v4: "10.48.7.255".parse().unwrap(),
                        v6: "fd00::ffff:ffff:ffff:ffff".parse().ok(),
                    },
                )),
                forwards: vec![
                    forward(Some((target.clone(), "10.48.7.255".parse().ok()))),
                    forward(None),
                ],
                hold: vec![("198.51.100.2".parse().unwrap(), "eth1".into())],
                ingress: vec!["198.51.100.1".parse().unwrap()],
                proxied: vec![proxied(Some(target.clone())), proxied(None)],
            }],
            sources: vec![Source {
                node: Uuid::from_u128(2),
                mesh_address: "10.48.11.255".parse().unwrap(),
                admits: vec![Admit {
                    address: "10.48.0.2".parse().unwrap(),
                    port: 80,
                    protocol: Protocol::Tcp,
                }],
                proxied: vec![Admit {
                    address: "10.48.0.3".parse().unwrap(),
                    port: 8080,
                    protocol: Protocol::Tcp,
                }],
            }],
            challenges: vec![HttpChallenge { token: "t".into(), key_authorization: "t.k".into() }],
            alpn_challenges: vec![AlpnChallenge {
                name: "shop.example.com".into(),
                digest: vec![7; 32],
            }],
        };
        assert_eq!(node_edges_from_pb(&node_edges_to_pb(&told)).unwrap(), told);
    }
}
