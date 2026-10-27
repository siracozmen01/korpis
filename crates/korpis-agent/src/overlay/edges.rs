// SPDX-License-Identifier: AGPL-3.0-or-later

//! What the node keeps of edges (§3.1 of docs/07-networking.md): the table the control plane
//! last told it, kept on disk beside the mesh's members so forwarding runs with the control plane
//! gone, and asked of the helper with the mesh. As a member: every address and port of its
//! groups' tables translated, each into the GRE link of its workload's node, and the probe port
//! of its own address into the link of the node the probe is answered by, and ports 80 and 443
//! of each group's ingress address to its own proxy. As a node behind edges: a link to each that
//! forwards here, and what each may reach here through it or, from its proxy, through the mesh.

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

fn ipv6(text: &str) -> bool {
    text.parse::<IpAddr>().is_ok_and(|a| a.is_ipv6())
}

/// The probe port of each of `probed`, translated into the link of the probe node, whose mesh
/// address of the address's own family `g` names: IPv4 to its IPv4 address and IPv6 to its IPv6
/// one, both carried in the one link; an address of a family it has no mesh address of is not
/// translated, and its probe goes unanswered.
fn probes<'a>(
    g: &agent::EdgeForwarding,
    probed: impl Iterator<Item = &'a String>,
    tunnels: &mut BTreeSet<String>,
) -> Vec<helper::EdgeTranslation> {
    let mut out = Vec::new();
    if g.probe_mesh_address.is_empty() {
        return out;
    }
    for address in probed {
        let to = if ipv4(address) { &g.probe_mesh_address } else { &g.probe_mesh_address_v6 };
        if to.is_empty() {
            continue;
        }
        tunnels.insert(g.probe_mesh_address.clone());
        out.push(helper::EdgeTranslation {
            address: address.clone(),
            port: g.probe_port,
            transport: helper::Transport::TRANSPORT_UDP.into(),
            via: g.probe_mesh_address.clone(),
            to_address: to.clone(),
            to_port: g.probe_port,
            ..Default::default()
        });
    }
    out
}

/// What one group's forwarding asks of the helper, this member advertising `own`: each port of
/// the group's table, IPv4 and IPv6 alike, and the probe port of its own first advertised address
/// of each family and of each address it is named to hold; each mesh address it tunnels to added
/// to `tunnels`.
fn forwarding(
    g: &agent::EdgeForwarding,
    own: &[String],
    tunnels: &mut BTreeSet<String>,
) -> Vec<helper::EdgeTranslation> {
    let mut out = Vec::new();
    for f in &g.forwards {
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
    let first = own.iter().find(|a| ipv4(a)).into_iter().chain(own.iter().find(|a| ipv6(a)));
    let held = g.hold.iter().map(|h| &h.address);
    let probed: BTreeSet<&String> = first.chain(held).collect();
    out.extend(probes(g, probed.into_iter(), tunnels));
    out
}

/// Ports 80 and 443 of each of `g`'s ingress addresses over TCP, and 443 over UDP for HTTP/3,
/// translated to the same port of this member's own first advertised address of the address's
/// family, where its proxy listens; an address of a family it advertises none of is not
/// translated, and is refused.
fn to_proxy(g: &agent::EdgeForwarding, own: &[String]) -> Vec<helper::EdgeTranslation> {
    let mut out = Vec::new();
    for address in &g.ingress {
        let v4 = ipv4(address);
        let Some(proxy) = own.iter().find(|a| if v4 { ipv4(a) } else { ipv6(a) }) else {
            continue;
        };
        let tcp = helper::Transport::TRANSPORT_TCP;
        let ports = [(80, tcp), (443, tcp), (443, helper::Transport::TRANSPORT_UDP)];
        out.extend(ports.map(|(port, transport)| helper::EdgeTranslation {
            address: address.clone(),
            port,
            transport: transport.into(),
            via: String::new(),
            to_address: proxy.clone(),
            to_port: port,
            ..Default::default()
        }));
    }
    out
}

/// What the edges' proxies may reach here through the mesh, each from its mesh address.
pub(super) fn proxied(told: &agent::Edges) -> Vec<helper::OverlayAdmitted> {
    let mut out = Vec::new();
    for s in &told.sources {
        out.extend(s.proxied.iter().map(|a| helper::OverlayAdmitted {
            source: s.mesh_address.clone(),
            destination: a.workload_address.clone(),
            port: a.container_port,
            transport: helper::Transport::TRANSPORT_TCP.into(),
            ..Default::default()
        }));
    }
    out
}

/// What `told` asks of the helper, this node advertising `own` and holding the addresses of
/// `holding`, each under a lease valid now.
pub(super) fn table(
    told: &agent::Edges,
    own: &[String],
    holding: &super::leases::Holding,
) -> helper::EdgeTable {
    let mut tunnels = BTreeSet::new();
    let mut translations: Vec<helper::EdgeTranslation> =
        told.groups.iter().flat_map(|g| forwarding(g, own, &mut tunnels)).collect();
    translations.extend(told.groups.iter().flat_map(|g| to_proxy(g, own)));
    let mut admits = Vec::new();
    // A member that only proxies here needs no link: its proxy comes through the mesh.
    for s in told.sources.iter().filter(|s| !s.admits.is_empty()) {
        tunnels.insert(s.mesh_address.clone());
        admits.extend(s.admits.iter().map(|a| helper::EdgeAdmit {
            source: s.mesh_address.clone(),
            destination: a.workload_address.clone(),
            port: a.container_port,
            transport: transport(a.protocol).into(),
            ..Default::default()
        }));
    }
    let holds = holding.iter().map(|(address, (interface, _))| helper::EdgeHold {
        address: address.clone(),
        interface: interface.clone(),
        ..Default::default()
    });
    helper::EdgeTable {
        tunnels: tunnels.into_iter().collect(),
        translations,
        holds: holds.collect(),
        admits,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A member's table: one IPv4 port forwarded to node A, one IPv6 port refused, and one edge
    /// forwarding to it.
    fn told() -> agent::Edges {
        agent::Edges {
            mesh_address: "10.48.3.255".into(),
            groups: vec![agent::EdgeForwarding {
                group: "egr_1".into(),
                probe_port: 65_535,
                probe_node: "nd_a".into(),
                probe_mesh_address: "10.48.7.255".into(),
                probe_mesh_address_v6: "fd00::ffff:ffff:ffff:ffff".into(),
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
        }
    }

    #[test]
    fn a_member_translates_its_group_and_its_own_probe_port_and_a_node_admits_its_edges() {
        let told = told();
        let holding = super::super::leases::Holding::from([
            ("198.51.100.2".to_owned(), ("eth1".to_owned(), 7)),
            ("2001:db8::2".to_owned(), (String::new(), 8)),
        ]);
        let own = ["203.0.113.5".into(), "2001:db8:5::5".into()];
        let t = table(&told, &own, &holding);
        let held: Vec<(&str, &str)> =
            t.holds.iter().map(|h| (h.address.as_str(), h.interface.as_str())).collect();
        assert_eq!(held, [("198.51.100.2", "eth1"), ("2001:db8::2", "")], "both families");
        assert_eq!(t.tunnels, ["10.48.11.255", "10.48.7.255"]);
        let translated: Vec<(&str, &str, &str)> = t
            .translations
            .iter()
            .map(|t| (t.address.as_str(), t.via.as_str(), t.to_address.as_str()))
            .collect();
        let probe = ("10.48.7.255", "fd00::ffff:ffff:ffff:ffff");
        assert_eq!(
            translated,
            [
                ("198.51.100.2", "10.48.7.255", "10.48.4.2"),
                ("2001:db8::2", "", ""),
                ("2001:db8:5::5", probe.0, probe.1),
                ("203.0.113.5", probe.0, probe.0),
            ],
            "the group's ports, refused where they run nowhere, and its own probe of each family"
        );
        assert_eq!(t.admits[0].transport.as_known(), Some(helper::Transport::TRANSPORT_TCP));
    }

    #[test]
    fn a_member_sends_its_ingress_ports_to_its_proxy_and_a_node_admits_proxies_through_the_mesh() {
        let mut told = told();
        told.groups[0].forwards.clear();
        told.groups[0].ingress = vec!["198.51.100.1".into(), "2001:db8::1".into()];
        told.sources.push(agent::EdgeSource {
            node: "nd_e3".into(),
            mesh_address: "10.48.15.255".into(),
            proxied: vec![agent::EdgeAdmit {
                workload_address: "10.48.0.3".into(),
                container_port: 8080,
                ..Default::default()
            }],
            ..Default::default()
        });
        let own = ["203.0.113.5".into()];
        let t = table(&told, &own, &super::super::leases::Holding::new());
        let proxy: Vec<(&str, u32, bool, &str)> = t
            .translations
            .iter()
            .filter(|t| t.address == "198.51.100.1" || t.address == "2001:db8::1")
            .filter(|t| t.port != 65_535)
            .map(|t| {
                let udp = t.transport.as_known() == Some(helper::Transport::TRANSPORT_UDP);
                assert!(t.via.is_empty(), "to its own proxy, through no link");
                (t.address.as_str(), t.port, udp, t.to_address.as_str())
            })
            .collect();
        let to = |port, udp| ("198.51.100.1", port, udp, "203.0.113.5");
        let expected = [to(80, false), to(443, false), to(443, true)];
        assert_eq!(proxy, expected, "HTTP/3 too; no IPv6 address to listen on: refused");
        assert!(!t.tunnels.contains(&"10.48.15.255".to_owned()), "a proxy needs no link");
        let admitted = proxied(&told);
        assert_eq!(admitted.len(), 1);
        assert_eq!((admitted[0].source.as_str(), admitted[0].port), ("10.48.15.255", 8080));
    }
}
