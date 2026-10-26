// SPDX-License-Identifier: AGPL-3.0-or-later

//! What each node is told of edges (§3.1 of docs/07-networking.md): as a member of a group, the
//! group's whole forwarding table, so a member taking an address over has its forwarding in
//! place; and as a node an edge forwards to, each edge and what it may reach there. Pure: the
//! caller gives the groups, every forwarded address and port with where its workload runs, and
//! each mesh member's own address.

use super::EdgeGroup;
use crate::workload::spec::endpoints::Protocol;
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr};
use uuid::Uuid;

/// Where a forwarded port's workload runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Its node.
    pub node: Uuid,
    /// Its address there, as the node observed it.
    pub workload_address: IpAddr,
    /// Its port there.
    pub container_port: u16,
}

/// An address and port of a group's pools a `stable` endpoint holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The group whose pool it is of.
    pub group: Uuid,
    /// The address.
    pub address: IpAddr,
    /// The port.
    pub port: u16,
    /// The endpoint's protocol.
    pub protocol: Protocol,
    /// Where its workload runs; none where it is not running, and the port is refused.
    pub target: Option<Target>,
}

/// One forwarded port, as a member is told it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    /// The address.
    pub address: IpAddr,
    /// The port.
    pub port: u16,
    /// The endpoint's protocol.
    pub protocol: Protocol,
    /// Where it goes: the target, with its node's address in the mesh, none where it is the
    /// member itself; none at all where it is refused.
    pub target: Option<(Target, Option<Ipv4Addr>)>,
}

/// What a member forwards for one group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forwarding {
    /// The group.
    pub group: Uuid,
    /// The port forwarded for the control plane's probe.
    pub probe_port: u16,
    /// The node the probe is forwarded to, with its address in the mesh.
    pub probe: Option<(Uuid, Ipv4Addr)>,
    /// Every forwarded port.
    pub forwards: Vec<Forward>,
    /// Each address of the group it should hold, with the interface it puts the address on;
    /// empty where the address is routed to it.
    pub hold: Vec<(IpAddr, String)>,
}

/// What a node may be reached at by one edge's GRE link.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Admit {
    /// The address, a workload's or the node's own in the mesh for the probe.
    pub address: IpAddr,
    /// The port.
    pub port: u16,
    /// Over what.
    pub protocol: Protocol,
}

/// An edge that forwards to a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The edge.
    pub node: Uuid,
    /// Its address in the mesh, the other end of the GRE link.
    pub mesh_address: Ipv4Addr,
    /// What it may reach.
    pub admits: Vec<Admit>,
}

/// The member that should hold each address, with the interface of its pool on the members'
/// link, empty where its pool's router routes it.
pub type Holding = BTreeMap<IpAddr, (Uuid, String)>;

/// What one node is told of edges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeEdges {
    /// Its own address in the mesh.
    pub mesh_address: Option<Ipv4Addr>,
    /// Each group it is a member of.
    pub groups: Vec<Forwarding>,
    /// Each edge forwarding to it.
    pub sources: Vec<Source>,
}

/// The node a member forwards its group's probe to: the lowest of the nodes the group forwards to
/// but itself, or else of its other members, among the mesh's.
fn probe_node(
    g: &EdgeGroup,
    member: Uuid,
    entries: &[&Entry],
    mesh: &BTreeMap<Uuid, Ipv4Addr>,
) -> Option<(Uuid, Ipv4Addr)> {
    let targets: BTreeSet<Uuid> =
        entries.iter().filter_map(|e| e.target.as_ref().map(|t| t.node)).collect();
    let pick = |nodes: &mut dyn Iterator<Item = Uuid>| {
        nodes.filter(|n| *n != member).find_map(|n| mesh.get(&n).map(|a| (n, *a)))
    };
    pick(&mut targets.into_iter()).or_else(|| pick(&mut g.members.iter().copied()))
}

/// What `member` of `g` forwards: every entry of the group, each target with its node's address
/// in the mesh, and refused where that node is outside it; and the addresses of `holding` it
/// should hold.
fn forwarding(
    (g, member): (&EdgeGroup, Uuid),
    entries: &[&Entry],
    mesh: &BTreeMap<Uuid, Ipv4Addr>,
    holding: &Holding,
) -> Forwarding {
    let forwards = entries
        .iter()
        .map(|e| {
            let target = e.target.as_ref().and_then(|t| match mesh.get(&t.node) {
                _ if t.node == member => Some((t.clone(), None)),
                Some(a) => Some((t.clone(), Some(*a))),
                None => None,
            });
            Forward { address: e.address, port: e.port, protocol: e.protocol, target }
        })
        .collect();
    let probe = probe_node(g, member, entries, mesh);
    let ours = |a: &IpAddr| entries.iter().any(|e| e.address == *a);
    let hold = holding
        .iter()
        .filter(|(a, (m, _))| *m == member && ours(a))
        .map(|(a, (_, interface))| (*a, interface.clone()))
        .collect();
    Forwarding { group: g.id, probe_port: g.probe_port, probe, forwards, hold }
}

/// Where `f` goes through a tunnel, the node at its other end and what that node admits of it.
fn tunnelled(f: &Forward) -> Option<(Uuid, Admit)> {
    let (t, Some(_)) = f.target.as_ref()? else { return None };
    let admit = Admit { address: t.workload_address, port: t.container_port, protocol: f.protocol };
    Some((t.node, admit))
}

/// Every node's table: each member of a group that serves, in the mesh, is told the group's
/// forwarding; each node in the mesh a member forwards to is told that member, with the ports it
/// may reach there and the probe. A node outside the mesh is told nothing, since nothing reaches
/// it but through the mesh.
#[must_use]
pub fn tables(
    groups: &[EdgeGroup],
    entries: &[Entry],
    mesh: &BTreeMap<Uuid, Ipv4Addr>,
    holding: &Holding,
) -> BTreeMap<Uuid, NodeEdges> {
    let mut out: BTreeMap<Uuid, NodeEdges> = BTreeMap::new();
    let mut sources: BTreeMap<(Uuid, Uuid), BTreeSet<Admit>> = BTreeMap::new();
    for g in groups.iter().filter(|g| g.serves()) {
        let mine: Vec<&Entry> = entries.iter().filter(|e| e.group == g.id).collect();
        for &m in g.members.iter().filter(|m| mesh.contains_key(m)) {
            let f = forwarding((g, m), &mine, mesh, holding);
            for (node, admit) in f.forwards.iter().filter_map(tunnelled) {
                sources.entry((node, m)).or_default().insert(admit);
            }
            if let Some((n, a)) = f.probe {
                let admit =
                    Admit { address: IpAddr::V4(a), port: g.probe_port, protocol: Protocol::Udp };
                sources.entry((n, m)).or_default().insert(admit);
            }
            out.entry(m).or_default().groups.push(f);
        }
    }
    for ((node, edge), admits) in sources {
        let Some(&mesh_address) = mesh.get(&edge) else { continue };
        let source = Source { node: edge, mesh_address, admits: admits.into_iter().collect() };
        out.entry(node).or_default().sources.push(source);
    }
    for (node, edges) in &mut out {
        edges.mesh_address = mesh.get(node).copied();
    }
    out
}

#[cfg(test)]
mod tests;
