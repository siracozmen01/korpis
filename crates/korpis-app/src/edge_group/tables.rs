// SPDX-License-Identifier: AGPL-3.0-or-later

//! What every node is told of edges (§3.1 of docs/07-networking.md), computed from what the
//! control plane holds: every group, every address and port a `stable` endpoint holds, each
//! group's ingress addresses and the `ingress` endpoints it serves, where each such workload is
//! placed and whether its node observes it running there, each mesh member's own address, and
//! the challenges standing for the names each member's proxy serves.

use crate::App;
use crate::error::AppError;
use crate::ports::{
    CertificateWrite as _, Clock, EdgeLeaseWrite as _, RecipeSource, Store, Tx as _,
};
use korpis_core::edge::lease::assigned;
use korpis_core::edge::table::{
    Entry, Holding, Mesh, MeshView, NodeEdges, Proxied, Served, Target, tables,
};
use korpis_core::edge::{IngressAddress, StablePort, mesh_address, mesh_address_v6};
use korpis_core::workload::endpoint::Endpoint;
use korpis_core::workload::observation::ObservedState;
use korpis_core::workload::spec::endpoints::Exposure;
use korpis_core::workload::spec::endpoints::Protocol;
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use uuid::Uuid;

/// What every node is told of edges; a node absent is told nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EdgeTables(pub BTreeMap<Uuid, NodeEdges>);

impl EdgeTables {
    /// What `node` is told.
    #[must_use]
    pub fn of(&self, node: Uuid) -> Option<&NodeEdges> {
        self.0.get(&node)
    }
}

/// A workload as the edges forward to it: its endpoints, and where it runs, if it does, with its
/// IPv4 address there and its IPv6 address where it has one.
struct Placed {
    endpoints: Vec<Endpoint>,
    running: Option<(Uuid, Ipv4Addr, Option<Ipv6Addr>)>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Where `workload` runs: the node it is placed on, where that node observes it starting,
    /// running, or stopping, with the address it observed; and its endpoints.
    async fn placed_for_edges(&self, workload: Uuid) -> Result<Placed, AppError> {
        let endpoints = self.store.endpoints(workload).await?;
        let Some(p) = self.store.placement(workload).await? else {
            return Ok(Placed { endpoints, running: None });
        };
        let seen = self.store.observation(workload).await?.filter(|o| o.node == p.node);
        let live = |s: ObservedState| {
            matches!(s, ObservedState::Starting | ObservedState::Running | ObservedState::Stopping)
        };
        let running = seen
            .filter(|o| live(o.state))
            .and_then(|o| o.address.map(|a| (p.node, a, o.address_ipv6)));
        Ok(Placed { endpoints, running })
    }

    /// What every node is told of edges now.
    ///
    /// # Errors
    /// The store could not be read.
    pub async fn edge_tables(&self) -> Result<EdgeTables, AppError> {
        let groups = self.store.edge_groups_all().await?;
        if groups.is_empty() {
            return Ok(EdgeTables::default());
        }
        let addresses = self.mesh_addresses().await?;
        let colocated = self.store.colocated_nodes(self.clock.now()).await?;
        let held = self.store.stable_all().await?;
        let ingress = self.store.ingress_addresses_all().await?;
        let holding = self.holding(&groups, &held, &ingress).await?;
        let mut placed: BTreeMap<Uuid, Placed> = BTreeMap::new();
        let mut entries = Vec::with_capacity(held.len());
        for (group, port) in held {
            let p = self.placed_once(&mut placed, port.workload).await?;
            entries.push(entry(group, &port, p));
        }
        let mut proxied = Vec::new();
        for (workload, group) in self.store.ingress_served_all().await? {
            let p = self.placed_once(&mut placed, workload).await?;
            proxied.extend(proxied_of(group, p));
        }
        let view = MeshView { addresses: &addresses, colocated: &colocated };
        let served = Served { entries: &entries, ingress: &ingress, proxied: &proxied };
        let mut all = tables(&groups, served, view, &holding);
        self.with_challenges(&mut all).await?;
        Ok(EdgeTables(all))
    }

    /// Where `workload` runs, read once for every table.
    async fn placed_once<'a>(
        &self,
        placed: &'a mut BTreeMap<Uuid, Placed>,
        workload: Uuid,
    ) -> Result<&'a Placed, AppError> {
        Ok(match placed.entry(workload) {
            std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::btree_map::Entry::Vacant(v) => {
                v.insert(self.placed_for_edges(workload).await?)
            }
        })
    }

    /// Gives each member the challenges standing for the certificates of the names its groups
    /// serve, which its proxy answers.
    async fn with_challenges(&self, all: &mut BTreeMap<Uuid, NodeEdges>) -> Result<(), AppError> {
        let workloads = |edges: &NodeEdges| -> BTreeSet<Uuid> {
            let proxied = edges.groups.iter().flat_map(|g| &g.proxied);
            proxied.map(|p| p.endpoint.workload).collect()
        };
        let every: BTreeSet<Uuid> = all.values().flat_map(workloads).collect();
        let mut tx = self.store.begin().await?;
        let mut of = BTreeMap::new();
        for w in every {
            of.insert(w, (tx.challenges_for(w).await?, tx.alpn_challenges_for(w).await?));
        }
        tx.commit().await?;
        for edges in all.values_mut() {
            let standing: Vec<_> = workloads(edges).iter().filter_map(|w| of.get(w)).collect();
            edges.challenges.extend(standing.iter().flat_map(|(http, _)| http.iter().cloned()));
            let alpn = standing.iter().flat_map(|(_, alpn)| alpn.iter().cloned());
            edges.alpn_challenges.extend(alpn);
            edges.challenges.sort_by(|a, b| a.token.cmp(&b.token));
            edges.challenges.dedup();
            edges.alpn_challenges.sort_by(|a, b| a.name.cmp(&b.name));
            edges.alpn_challenges.dedup();
        }
        Ok(())
    }

    /// Each mesh member's own addresses in the mesh.
    ///
    /// # Errors
    /// The store could not be read.
    pub async fn mesh_addresses(&self) -> Result<BTreeMap<Uuid, Mesh>, AppError> {
        let mesh = self.mesh().await?;
        Ok(mesh
            .members
            .iter()
            .filter_map(|m| {
                let v4 = mesh_address(&m.pools)?;
                Some((m.node, Mesh { v4, v6: mesh_address_v6(&m.pools) }))
            })
            .collect())
    }

    /// Records that `replica` shares its network with each of `nodes`, for `hold` from now, and
    /// with no other node; an edge group's probe then answers through another node where it can.
    ///
    /// # Errors
    /// The store refused it.
    pub async fn record_colocated(
        &self,
        replica: Uuid,
        nodes: &[Uuid],
        hold: std::time::Duration,
    ) -> Result<(), AppError> {
        let until = self.clock.now() + hold;
        let mut tx = self.store.begin().await?;
        tx.record_colocated(replica, nodes, until).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The member that should hold each address `held` names and each of `ingress`, with its
    /// pool's interface: an ingress address one of the members serving the ingress alone.
    async fn holding(
        &self,
        groups: &[korpis_core::edge::EdgeGroup],
        held: &[(Uuid, StablePort)],
        ingress: &[IngressAddress],
    ) -> Result<Holding, AppError> {
        let in_use: BTreeSet<(Uuid, IpAddr)> = held.iter().map(|(g, p)| (*g, p.address)).collect();
        let in_use: Vec<(Uuid, IpAddr)> = in_use.into_iter().collect();
        let states = self.store.edge_members_all().await?;
        let holds = self.store.edge_holds_all().await?;
        let now = self.clock.now();
        let mut who = assigned(groups, &in_use, (&states, &holds), now);
        let mut proxying = Vec::with_capacity(groups.len());
        for g in groups {
            proxying.push(self.proxying_group(g).await?);
        }
        let named: Vec<(Uuid, IpAddr)> = ingress.iter().map(|i| (i.group, i.address)).collect();
        who.extend(assigned(&proxying, &named, (&states, &holds), now));
        let pools = held.iter().map(|(_, p)| (p.address, p.pool));
        let pools = pools.chain(ingress.iter().map(|i| (i.address, i.pool)));
        let mut interfaces: BTreeMap<Uuid, String> = BTreeMap::new();
        let mut out = Holding::new();
        for (address, pool) in pools {
            let Some(member) = who.get(&address) else { continue };
            let interface = match interfaces.entry(pool) {
                std::collections::btree_map::Entry::Occupied(o) => o.get().clone(),
                std::collections::btree_map::Entry::Vacant(v) => {
                    let pool = self.store.address_pool(pool).await?;
                    v.insert(pool.map(|p| p.interface).unwrap_or_default()).clone()
                }
            };
            out.insert(address, (*member, interface));
        }
        Ok(out)
    }
}

/// The routes of `group` for `p`'s `ingress` endpoints, each to its workload's IPv4 address where
/// it runs.
fn proxied_of(group: Uuid, p: &Placed) -> Vec<Proxied> {
    let routed =
        p.endpoints.iter().filter(|e| e.exposure == Exposure::Ingress && !e.domains.is_empty());
    routed
        .map(|e| Proxied {
            group,
            endpoint: e.clone(),
            target: p.running.map(|(node, v4, _)| Target {
                node,
                workload_address: IpAddr::V4(v4),
                container_port: e.container_port,
            }),
        })
        .collect()
}

/// The entry for `port` of `group`, whose workload is `p`: to the workload's address of the
/// port's family; refused where it is not running, where its endpoint is not yet placed with it,
/// or where the port is IPv6 and the workload has no IPv6 address.
fn entry(group: Uuid, port: &StablePort, p: &Placed) -> Entry {
    let endpoint = p.endpoints.iter().find(|e| e.name == port.endpoint);
    let target = endpoint.zip(p.running).and_then(|(e, (node, v4, v6))| {
        let workload_address = match port.address {
            IpAddr::V4(_) => IpAddr::V4(v4),
            IpAddr::V6(_) => IpAddr::V6(v6?),
        };
        Some(Target { node, workload_address, container_port: e.container_port })
    });
    Entry {
        group,
        address: port.address,
        port: port.port,
        // A port with no target is refused over every transport, whatever this says.
        protocol: endpoint.map_or(Protocol::Tcp, |e| e.protocol),
        target,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failing test should fail loudly")]

    use super::*;
    use korpis_core::workload::endpoint::Transport;

    #[test]
    fn a_port_goes_to_the_workload_s_address_of_its_family_and_ipv6_without_one_is_refused() {
        let (node, workload) = (Uuid::from_u128(3), Uuid::from_u128(7));
        let game = Endpoint {
            id: Uuid::from_u128(8),
            workload,
            name: "game".into(),
            protocol: Protocol::Udp,
            container_port: 25_565,
            exposure: Exposure::Stable,
            node,
            port: None,
            domains: Vec::new(),
            tls: None,
            certificate: None,
        };
        let port = |address: &str| StablePort {
            address: address.parse().unwrap(),
            port: 25_565,
            transport: Transport::Udp,
            pool: Uuid::from_u128(9),
            workload,
            endpoint: "game".into(),
        };
        let placed = |v6: Option<&str>| Placed {
            endpoints: vec![game.clone()],
            running: Some((node, "10.48.4.2".parse().unwrap(), v6.map(|a| a.parse().unwrap()))),
        };
        let group = Uuid::from_u128(10);
        let to = |e: Entry| e.target.map(|t| t.workload_address.to_string());
        let dual = placed(Some("fd00::2"));
        assert_eq!(to(entry(group, &port("198.51.100.2"), &dual)).as_deref(), Some("10.48.4.2"));
        assert_eq!(to(entry(group, &port("2001:db8::2"), &dual)).as_deref(), Some("fd00::2"));
        let v4 = placed(None);
        assert_eq!(to(entry(group, &port("2001:db8::2"), &v4)), None, "no IPv6 to go to: refused");
    }
}
