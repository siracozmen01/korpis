// SPDX-License-Identifier: AGPL-3.0-or-later

//! The mesh between nodes (§9.1 of docs/07-networking.md): who is a member, and which pairs of
//! members reach each other. Every node that reports a mesh interface is a member, unless one of
//! its pools overlaps a pool of a node enrolled before it, which would leave the mesh two routes
//! to one address; that node is outside, and says with which. A pair is reachable when each of
//! its nodes reports a handshake with the other younger than WireGuard's Reject-After-Time, past
//! which a session carries no data, so the control plane joins two reports neither node can make
//! alone.

use ipnet::IpNet;
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// A WireGuard public key.
pub type Key = [u8; 32];

/// The age past which a session carries no data: WireGuard's Reject-After-Time (§5 of
/// docs/research/evidence.md).
pub const REJECT_AFTER: Duration = Duration::from_secs(180);

/// What a node reports of its side of the mesh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayFacts {
    /// Its interface's public key.
    pub key: Key,
    /// The UDP port its interface listens on.
    pub port: u16,
    /// Its workloads' pools.
    pub pools: Vec<IpNet>,
    /// When each peer, by key, last completed a handshake with it, as it last read them.
    pub handshakes: BTreeMap<Key, SystemTime>,
}

/// A node as the mesh sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The node.
    pub node: Uuid,
    /// When it enrolled, which decides which of two overlapping nodes stays.
    pub enrolled_at: SystemTime,
    /// The addresses it advertises (§9 of docs/07-networking.md); the first is its endpoint.
    pub addresses: Vec<IpAddr>,
    /// What it reports of the mesh.
    pub facts: OverlayFacts,
}

/// A member of the mesh, as each other member is told of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The node.
    pub node: Uuid,
    /// Its interface's public key.
    pub key: Key,
    /// Where it is reached; none for a node that advertises no address, which is reached once
    /// it has reached its peer.
    pub endpoint: Option<SocketAddr>,
    /// Its pools: where packets to it are routed, and the only sources accepted from it.
    pub pools: Vec<IpNet>,
}

/// The mesh: its members, and the nodes outside it with why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mesh {
    /// Every member, in the order they enrolled.
    pub members: Vec<Member>,
    /// Each node outside it, and why.
    pub outside: Vec<(Uuid, String)>,
}

fn overlaps(a: &[IpNet], b: &[IpNet]) -> bool {
    a.iter().any(|x| b.iter().any(|y| x.contains(&y.network()) || y.contains(&x.network())))
}

/// The mesh of `candidates`: each a member unless a pool of it overlaps one of a member enrolled
/// before it.
#[must_use]
pub fn mesh(mut candidates: Vec<Candidate>) -> Mesh {
    candidates.sort_by_key(|c| (c.enrolled_at, c.node));
    let mut out = Mesh::default();
    for c in candidates {
        if let Some(m) = out.members.iter().find(|m| overlaps(&m.pools, &c.facts.pools)) {
            let why = format!("its workload pool overlaps node {}'s", m.node);
            out.outside.push((c.node, why));
            continue;
        }
        let endpoint = c.addresses.first().map(|a| SocketAddr::new(*a, c.facts.port));
        out.members.push(Member { node: c.node, key: c.facts.key, endpoint, pools: c.facts.pools });
    }
    out
}

/// Whether `a` and `b` reach each other at `now`: each reports a handshake with the other's key
/// younger than [`REJECT_AFTER`].
#[must_use]
pub fn reachable(a: &OverlayFacts, b: &OverlayFacts, now: SystemTime) -> bool {
    let fresh = |from: &OverlayFacts, key: &Key| {
        from.handshakes
            .get(key)
            .is_some_and(|at| now.duration_since(*at).unwrap_or(Duration::ZERO) < REJECT_AFTER)
    };
    fresh(a, &b.key) && fresh(b, &a.key)
}

impl FromIterator<(Uuid, Uuid)> for Pairs {
    fn from_iter<I: IntoIterator<Item = (Uuid, Uuid)>>(pairs: I) -> Self {
        Self(pairs.into_iter().map(|(a, b)| (a.min(b), a.max(b))).collect())
    }
}

/// A node's place in the mesh, as the API shows it (§3.4 of docs/01-model.md).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Standing {
    /// It is a member and reaches every other member.
    pub available: bool,
    /// Its side of the mesh as its agent last reported it; none for a node reporting none.
    pub facts: Option<OverlayFacts>,
    /// Each other member, and whether the pair reaches each other now; none for a node outside.
    pub peers: Vec<(Uuid, bool)>,
}

impl Standing {
    /// `node`'s place in `mesh` at `now`, by what `candidates` report.
    #[must_use]
    pub fn of(node: Uuid, (mesh, candidates): (&Mesh, &[Candidate]), now: SystemTime) -> Self {
        let facts = |n: Uuid| candidates.iter().find(|c| c.node == n).map(|c| &c.facts);
        let own = facts(node).cloned();
        let member = mesh.members.iter().any(|m| m.node == node);
        let peers: Vec<(Uuid, bool)> = match own.as_ref().filter(|_| member) {
            None => Vec::new(),
            Some(own) => mesh
                .members
                .iter()
                .filter(|m| m.node != node)
                .map(|m| (m.node, facts(m.node).is_some_and(|f| reachable(own, f, now))))
                .collect(),
        };
        Self { available: member && peers.iter().all(|(_, r)| *r), facts: own, peers }
    }
}

/// The pairs of members that reach each other, which the scheduler asks of a dependency across
/// nodes (§2.1 of docs/05-scheduling.md).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pairs(std::collections::BTreeSet<(Uuid, Uuid)>);

impl Pairs {
    /// No pairs: a fleet with no mesh.
    pub const EMPTY: Self = Self(std::collections::BTreeSet::new());

    /// The pairs of `mesh`'s members that reach each other at `now`, by what `candidates` report.
    #[must_use]
    pub fn of(mesh: &Mesh, candidates: &[Candidate], now: SystemTime) -> Self {
        let facts = |n: Uuid| candidates.iter().find(|c| c.node == n).map(|c| &c.facts);
        let all = &mesh.members;
        let pairs =
            all.iter().enumerate().flat_map(|(i, a)| all.iter().skip(i + 1).map(move |b| (a, b)));
        let out = pairs
            .filter(|(a, b)| {
                facts(a.node).zip(facts(b.node)).is_some_and(|(x, y)| reachable(x, y, now))
            })
            .map(|(a, b)| (a.node.min(b.node), a.node.max(b.node)))
            .collect();
        Self(out)
    }

    /// Whether `a` and `b` reach each other through the mesh.
    #[must_use]
    pub fn joins(&self, a: Uuid, b: Uuid) -> bool {
        self.0.contains(&(a.min(b), a.max(b)))
    }
}

#[cfg(test)]
mod tests;
