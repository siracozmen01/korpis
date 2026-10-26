// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the node keeps of edges (§3.1 of docs/07-networking.md): the table the control plane
//! last told it, kept on disk beside the mesh's members so forwarding runs with the control plane
//! gone, and asked of the helper with the mesh. As a member: every address and port of its
//! groups' tables translated, each into the GRE link of its workload's node, and the probe port
//! of its own address into the link of the node the probe is answered by. As a node behind
//! edges: a link to each, and what each may reach here.

use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::helper::v1 as helper;
use korpis_proto::korpis::v1 as pb;
use std::collections::BTreeSet;
use std::net::IpAddr;

/// The file in the state directory that holds the table last told.
pub(super) const EDGES: &str = "edges.binpb";

fn transport(p: buffa::EnumValue<pb::EndpointProtocol>) -> helper::Transport {
    match p.as_known() {
        Some(
            pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP
            | pb::EndpointProtocol::ENDPOINT_PROTOCOL_QUIC,
        ) => helper::Transport::TRANSPORT_UDP,
        _ => helper::Transport::TRANSPORT_TCP,
    }
}

fn ipv4(text: &str) -> bool {
    text.parse::<IpAddr>().is_ok_and(|a| a.is_ipv4())
}

/// What one group's forwarding asks of the helper, this member advertising `own`; each mesh
/// address it tunnels to added to `tunnels`. An IPv6 stable address is not forwarded yet (P3.7e
/// in §4 of docs/20-roadmap.md).
fn forwarding(
    g: &agent::EdgeForwarding,
    own: &[String],
    tunnels: &mut BTreeSet<String>,
) -> Vec<helper::EdgeTranslation> {
    let mut out = Vec::new();
    for f in g.forwards.iter().filter(|f| ipv4(&f.address)) {
        if !f.mesh_address.is_empty() {
            tunnels.insert(f.mesh_address.clone());
        }
        out.push(helper::EdgeTranslation {
            address: f.address.clone(),
            port: f.port,
            transport: transport(f.protocol).into(),
            via: f.mesh_address.clone(),
            to_address: f.workload_address.clone(),
            to_port: f.container_port,
            ..Default::default()
        });
    }
    let first = own.iter().find(|a| ipv4(a));
    if let (Some(address), false) = (first, g.probe_mesh_address.is_empty()) {
        tunnels.insert(g.probe_mesh_address.clone());
        out.push(helper::EdgeTranslation {
            address: address.clone(),
            port: g.probe_port,
            transport: helper::Transport::TRANSPORT_UDP.into(),
            via: g.probe_mesh_address.clone(),
            to_address: g.probe_mesh_address.clone(),
            to_port: g.probe_port,
            ..Default::default()
        });
    }
    out
}

/// What `told` asks of the helper, this node advertising `own`.
pub(super) fn table(told: &agent::Edges, own: &[String]) -> helper::EdgeTable {
    let mut tunnels = BTreeSet::new();
    let translations = told.groups.iter().flat_map(|g| forwarding(g, own, &mut tunnels)).collect();
    let mut admits = Vec::new();
    for s in &told.sources {
        tunnels.insert(s.mesh_address.clone());
        admits.extend(s.admits.iter().filter(|a| ipv4(&a.workload_address)).map(|a| {
            helper::EdgeAdmit {
                source: s.mesh_address.clone(),
                destination: a.workload_address.clone(),
                port: a.container_port,
                transport: transport(a.protocol).into(),
                ..Default::default()
            }
        }));
    }
    helper::EdgeTable {
        tunnels: tunnels.into_iter().collect(),
        translations,
        admits,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_translates_its_group_and_its_own_probe_port_and_a_node_admits_its_edges() {
        let told = agent::Edges {
            mesh_address: "10.48.3.255".into(),
            groups: vec![agent::EdgeForwarding {
                group: "egr_1".into(),
                probe_port: 65_535,
                probe_node: "nd_a".into(),
                probe_mesh_address: "10.48.7.255".into(),
                forwards: vec![
                    agent::EdgeForward {
                        address: "198.51.100.2".into(),
                        port: 25_565,
                        protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_UDP.into(),
                        node: "nd_a".into(),
                        mesh_address: "10.48.7.255".into(),
                        workload_address: "10.48.4.2".into(),
                        container_port: 25_565,
                        ..Default::default()
                    },
                    agent::EdgeForward {
                        address: "2001:db8::2".into(),
                        port: 25_565,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }],
            sources: vec![agent::EdgeSource {
                node: "nd_e2".into(),
                mesh_address: "10.48.11.255".into(),
                admits: vec![agent::EdgeAdmit {
                    workload_address: "10.48.0.2".into(),
                    container_port: 80,
                    protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_HTTP.into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let t = table(&told, &["203.0.113.5".into()]);
        assert_eq!(t.tunnels, ["10.48.11.255", "10.48.7.255"]);
        assert_eq!(t.translations.len(), 2, "IPv4 alone, and the probe of its own address");
        let probe = &t.translations[1];
        assert_eq!(
            (probe.address.as_str(), probe.to_address.as_str()),
            ("203.0.113.5", "10.48.7.255")
        );
        assert_eq!(t.admits[0].transport.as_known(), Some(helper::Transport::TRANSPORT_TCP));
    }
}
