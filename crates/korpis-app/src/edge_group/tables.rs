// SPDX-License-Identifier: AGPL-3.0-or-later

//! What every node is told of edges (§3.1 of docs/07-networking.md), computed from what the
//! control plane holds: every group, every address and port a `stable` endpoint holds, where
//! each such workload is placed and whether its node observes it running there, and each mesh
//! member's own address.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::edge::table::{Entry, NodeEdges, Target, tables};
use korpis_core::edge::{StablePort, mesh_address};
use korpis_core::workload::endpoint::Endpoint;
use korpis_core::workload::observation::ObservedState;
use korpis_core::workload::spec::endpoints::Protocol;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;
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

/// A workload as the edges forward to it: its endpoints, and where it runs, if it does.
struct Placed {
    endpoints: Vec<Endpoint>,
    running: Option<(Uuid, std::net::IpAddr)>,
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
            .and_then(|o| o.address.map(|a| (p.node, std::net::IpAddr::V4(a))));
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
        let mesh = self.mesh().await?;
        let addresses: BTreeMap<Uuid, Ipv4Addr> = mesh
            .members
            .iter()
            .filter_map(|m| mesh_address(&m.pools).map(|a| (m.node, a)))
            .collect();
        let held = self.store.stable_all().await?;
        let mut placed: BTreeMap<Uuid, Placed> = BTreeMap::new();
        let mut entries = Vec::with_capacity(held.len());
        for (group, port) in held {
            let p = match placed.entry(port.workload) {
                std::collections::btree_map::Entry::Occupied(o) => o.into_mut(),
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(self.placed_for_edges(port.workload).await?)
                }
            };
            entries.push(entry(group, &port, p));
        }
        Ok(EdgeTables(tables(&groups, &entries, &addresses)))
    }
}

/// The entry for `port` of `group`, whose workload is `p`: refused where it is not running, or
/// where its endpoint is not yet placed with it.
fn entry(group: Uuid, port: &StablePort, p: &Placed) -> Entry {
    let endpoint = p.endpoints.iter().find(|e| e.name == port.endpoint);
    let target = endpoint.zip(p.running).map(|(e, (node, workload_address))| Target {
        node,
        workload_address,
        container_port: e.container_port,
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
