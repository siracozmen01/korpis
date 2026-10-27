// SPDX-License-Identifier: AGPL-3.0-or-later

//! What each node is told of edges (§3.1 of docs/07-networking.md): as a member of a group, the
//! group's whole forwarding table, so a member taking an address over has its forwarding in
//! place, with the group's ingress addresses and every route its proxy serves there; and as a
//! node an edge forwards to, each edge and what it may reach there, through its GRE link or, for
//! its proxy, through the mesh. Pure: the caller gives the groups, every forwarded address and
//! port and every `ingress` endpoint a group serves, with where its workload runs, and each mesh
//! member's own address.

use super::{EdgeGroup, IngressAddress};
use crate::workload::endpoint::Endpoint;
use crate::workload::spec::endpoints::Protocol;
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use uuid::Uuid;

/// A node's own addresses in the mesh: the last of its IPv4 pool, from which its GRE links are
/// made, and the last of its unique local prefix where it has one, at which it answers the probe
/// of an IPv6 address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mesh {
    /// Its IPv4 address in the mesh.
    pub v4: Ipv4Addr,
    /// Its IPv6 address in the mesh.
    pub v6: Option<Ipv6Addr>,
}

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

/// An `ingress` endpoint a group serves (§3.1 of docs/07-networking.md), routed by every
/// member's proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proxied {
    /// The group.
    pub group: Uuid,
    /// The endpoint, with the names it routes and how.
    pub endpoint: Endpoint,
    /// Where its workload runs, at its IPv4 address; none where it is not running, and the
    /// route is left out.
    pub target: Option<Target>,
}

/// What the groups serve: every address and port a `stable` endpoint holds, each group's ingress
/// addresses, and every `ingress` endpoint a group serves.
#[derive(Debug, Clone, Copy)]
pub struct Served<'a> {
    /// The `stable` endpoints' addresses and ports.
    pub entries: &'a [Entry],
    /// The groups' ingress addresses.
    pub ingress: &'a [IngressAddress],
    /// The `ingress` endpoints.
    pub proxied: &'a [Proxied],
}

impl<'a> Served<'a> {
    /// The `stable` endpoints of `entries` alone.
    #[must_use]
    pub fn stable(entries: &'a [Entry]) -> Self {
        Self { entries, ingress: &[], proxied: &[] }
    }
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
    /// The node the probe is forwarded to, with its addresses in the mesh.
    pub probe: Option<(Uuid, Mesh)>,
    /// Every forwarded port.
    pub forwards: Vec<Forward>,
    /// Each address of the group it should hold, with the interface it puts the address on;
    /// empty where the address is routed to it.
    pub hold: Vec<(IpAddr, String)>,
    /// The group's ingress addresses, whose ports 80 and 443 go to the member's own proxy.
    pub ingress: Vec<IpAddr>,
    /// Every route its proxy serves for the group: each `ingress` endpoint, with its target
    /// where its workload runs on the member or a node of the mesh.
    pub proxied: Vec<Proxied>,
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
    /// What it may reach through its GRE link.
    pub admits: Vec<Admit>,
    /// What its ingress proxy may reach through the mesh, from its address there.
    pub proxied: Vec<Admit>,
}

/// The member that should hold each address, with the interface of its pool on the members'
/// link, empty where its pool's router routes it.
pub type Holding = BTreeMap<IpAddr, (Uuid, String)>;

/// What one node is told of edges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeEdges {
    /// Its own addresses in the mesh.
    pub mesh: Option<Mesh>,
    /// Each group it is a member of.
    pub groups: Vec<Forwarding>,
    /// Each edge forwarding to it.
    pub sources: Vec<Source>,
    /// The HTTP-01 challenges standing for the names its groups serve, which its proxy answers.
    pub challenges: Vec<crate::dns::certificate::HttpChallenge>,
    /// The TLS-ALPN-01 challenges standing for them.
    pub alpn_challenges: Vec<crate::dns::certificate::AlpnChallenge>,
}

/// The mesh's members with their addresses in it, and those sharing a machine and its network
/// with a control plane, whose answer to a probe that control plane would take locally.
#[derive(Debug, Clone, Copy)]
pub struct MeshView<'a> {
    /// Each member's addresses in the mesh.
    pub addresses: &'a BTreeMap<Uuid, Mesh>,
    /// The members a control plane shares its network with.
    pub colocated: &'a BTreeSet<Uuid>,
}

/// The node a member forwards its group's probe to: the lowest of the nodes the group forwards to
/// but itself, or else of its other members, among the mesh's; one no control plane shares a
/// network with first, since its answer to that control plane would never cross the link back,
/// and one that does only where there is no other.
fn probe_node(
    g: &EdgeGroup,
    member: Uuid,
    entries: &[&Entry],
    mesh: MeshView<'_>,
) -> Option<(Uuid, Mesh)> {
    let targets: BTreeSet<Uuid> =
        entries.iter().filter_map(|e| e.target.as_ref().map(|t| t.node)).collect();
    let pick = |apart: bool| {
        let candidates = targets.iter().copied().chain(g.members.iter().copied());
        candidates
            .filter(|n| *n != member && (!apart || !mesh.colocated.contains(n)))
            .find_map(|n| mesh.addresses.get(&n).map(|a| (n, *a)))
    };
    pick(true).or_else(|| pick(false))
}

/// What `member` of `g` forwards: every entry of the group, each target with its node's address
/// in the mesh, and refused where that node is outside it; the addresses of `holding` it should
/// hold; and the group's ingress addresses and routes, a route left out where its workload runs
/// on no node it reaches through the mesh.
fn forwarding(
    (g, member): (&EdgeGroup, Uuid),
    (entries, ingress, proxied): (&[&Entry], &[IpAddr], &[&Proxied]),
    view: MeshView<'_>,
    holding: &Holding,
) -> Forwarding {
    let mesh = view.addresses;
    let forwards = entries
        .iter()
        .map(|e| {
            let target = e.target.as_ref().and_then(|t| match mesh.get(&t.node) {
                _ if t.node == member => Some((t.clone(), None)),
                Some(a) => Some((t.clone(), Some(a.v4))),
                None => None,
            });
            Forward { address: e.address, port: e.port, protocol: e.protocol, target }
        })
        .collect();
    let probe = probe_node(g, member, entries, view);
    let ours = |a: &IpAddr| entries.iter().any(|e| e.address == *a) || ingress.contains(a);
    let hold = holding
        .iter()
        .filter(|(a, (m, _))| *m == member && ours(a))
        .map(|(a, (_, interface))| (*a, interface.clone()))
        .collect();
    let reached = |t: &Target| t.node == member || mesh.contains_key(&t.node);
    let proxied = proxied
        .iter()
        .map(|p| Proxied { target: p.target.clone().filter(reached), ..(*p).clone() })
        .collect();
    let (probe_port, ingress) = (g.probe_port, ingress.to_vec());
    Forwarding { group: g.id, probe_port, probe, forwards, hold, ingress, proxied }
}

/// Where `f` goes through a tunnel, the node at its other end and what that node admits of it.
fn tunnelled(f: &Forward) -> Option<(Uuid, Admit)> {
    let (t, Some(_)) = f.target.as_ref()? else { return None };
    let admit = Admit { address: t.workload_address, port: t.container_port, protocol: f.protocol };
    Some((t.node, admit))
}

/// Where `p`'s workload runs on another node than `member`, that node and what it admits of the
/// member's proxy.
fn proxied_to(p: &Proxied, member: Uuid) -> Option<(Uuid, Admit)> {
    let t = p.target.as_ref().filter(|t| t.node != member)?;
    let admit =
        Admit { address: t.workload_address, port: t.container_port, protocol: Protocol::Tcp };
    Some((t.node, admit))
}

/// What each node behind the members admits from each: through its GRE link, and from its proxy
/// through the mesh.
type Sources = BTreeMap<(Uuid, Uuid), (BTreeSet<Admit>, BTreeSet<Admit>)>;

/// Every node's table: each member of a group that serves, in the mesh, is told the group's
/// forwarding and routes; each node in the mesh a member forwards or proxies to is told that
/// member, with the ports it may reach there and the probe. A node outside the mesh is told
/// nothing, since nothing reaches it but through the mesh.
#[must_use]
pub fn tables(
    groups: &[EdgeGroup],
    served: Served<'_>,
    view: MeshView<'_>,
    holding: &Holding,
) -> BTreeMap<Uuid, NodeEdges> {
    let mesh = view.addresses;
    let mut out: BTreeMap<Uuid, NodeEdges> = BTreeMap::new();
    let mut sources = Sources::new();
    for g in groups.iter().filter(|g| g.serves()) {
        let mine: Vec<&Entry> = served.entries.iter().filter(|e| e.group == g.id).collect();
        let ingress: Vec<IpAddr> =
            served.ingress.iter().filter(|i| i.group == g.id).map(|i| i.address).collect();
        let proxied: Vec<&Proxied> = served.proxied.iter().filter(|p| p.group == g.id).collect();
        for &m in g.members.iter().filter(|m| mesh.contains_key(m)) {
            let f = forwarding((g, m), (&mine, &ingress, &proxied), view, holding);
            for (node, admit) in f.forwards.iter().filter_map(tunnelled) {
                sources.entry((node, m)).or_default().0.insert(admit);
            }
            for (node, admit) in f.proxied.iter().filter_map(|p| proxied_to(p, m)) {
                sources.entry((node, m)).or_default().1.insert(admit);
            }
            if let Some((n, a)) = f.probe {
                let at = std::iter::once(IpAddr::V4(a.v4)).chain(a.v6.map(IpAddr::V6));
                let admits = at.map(|address| Admit {
                    address,
                    port: g.probe_port,
                    protocol: Protocol::Udp,
                });
                sources.entry((n, m)).or_default().0.extend(admits);
            }
            out.entry(m).or_default().groups.push(f);
        }
    }
    for ((node, edge), (admits, proxied)) in sources {
        let Some(&at) = mesh.get(&edge) else { continue };
        let source = Source {
            node: edge,
            mesh_address: at.v4,
            admits: admits.into_iter().collect(),
            proxied: proxied.into_iter().collect(),
        };
        out.entry(node).or_default().sources.push(source);
    }
    for (node, edges) in &mut out {
        edges.mesh = mesh.get(node).copied();
    }
    out
}

#[cfg(test)]
mod tests;
