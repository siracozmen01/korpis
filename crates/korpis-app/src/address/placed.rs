// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload's dedicated address, given where it is placed (§3.7 of docs/07-networking.md):
//! under the placement lock, in the transaction that binds it, the lowest free address of a pool
//! that serves it and reaches its node, or the one it holds where that reaches the node already.

use crate::App;
use crate::error::AppError;
use crate::ports::{AddressWrite as _, Clock, RecipeSource, Store};
use korpis_core::address::{AddressPool, Taken, lowest_free};
use korpis_core::node::Node;
use korpis_core::workload::spec::IntentBody;
use korpis_core::workload::spec::endpoints::Exposure;
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use uuid::Uuid;

/// Whether `body` asks for a dedicated address: any endpoint of it exposed `dedicated_ip`.
pub(crate) fn asks(body: &IntentBody) -> bool {
    body.endpoints.iter().any(|e| e.exposure == Exposure::DedicatedIp)
}

/// What a workload's pools hold, as one transaction sees them.
struct Pools {
    /// Every pool of the organizations serving it, the nearest's first.
    serving: Vec<AddressPool>,
    /// What holds each address of each pool that has a row.
    taken: BTreeMap<Uuid, BTreeMap<IpAddr, Taken>>,
    /// The address it holds, with its pool.
    held: Option<(IpAddr, Uuid)>,
}

impl Pools {
    /// Whether the address it holds may be held on `node`.
    fn held_reaches(&self, node: &Node) -> Option<IpAddr> {
        let (address, pool) = self.held?;
        let p = self.serving.iter().find(|p| p.id == pool)?;
        p.reaches(node.id, node.organization).then_some(address)
    }

    /// The lowest free address of the first pool reaching `node` that has one.
    fn free_on(&self, node: &Node, now: std::time::SystemTime) -> Option<(Uuid, IpAddr)> {
        let empty = BTreeMap::new();
        self.serving.iter().filter(|p| p.reaches(node.id, node.organization)).find_map(|p| {
            lowest_free(p, self.taken.get(&p.id).unwrap_or(&empty), now).map(|a| (p.id, a))
        })
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    async fn pools(
        &self,
        tx: &mut S::Tx,
        workload: Uuid,
        owners: &[Uuid],
    ) -> Result<Pools, AppError> {
        let mut pools = Vec::new();
        for org in owners {
            pools.extend(tx.address_pools_now(*org).await?);
        }
        let mut taken = BTreeMap::new();
        for p in &pools {
            taken.insert(p.id, tx.pool_taken(p.id).await?);
        }
        let held = tx.dedicated_now(workload).await?;
        Ok(Pools { serving: pools, taken, held })
    }

    /// The nodes of `nodes` that `workload`, served by `owners`, may hold a dedicated address
    /// on; none where `body` asks for none, since the filter then asks nothing.
    pub(crate) async fn dedicated_nodes(
        &self,
        tx: &mut S::Tx,
        (workload, body): (Uuid, &IntentBody),
        owners: &[Uuid],
        nodes: &[Node],
    ) -> Result<BTreeSet<Uuid>, AppError> {
        if !asks(body) {
            return Ok(BTreeSet::new());
        }
        let pools = self.pools(tx, workload, owners).await?;
        let now = self.clock.now();
        let reach = |n: &&Node| pools.held_reaches(n).is_some() || pools.free_on(n, now).is_some();
        Ok(nodes.iter().filter(reach).map(|n| n.id).collect())
    }

    /// Gives `workload` the dedicated address `body` asks for on `node`, where it is bound now:
    /// the one it holds where that reaches the node, or else the lowest free one of a pool that
    /// does, the one it held going back to rest. Where `body` asks for none, it gives back any it
    /// holds. The filter found the node one, so finding none now is the store and the scheduler
    /// disagreeing.
    pub(crate) async fn hold_dedicated(
        &self,
        tx: &mut S::Tx,
        (workload, body): (Uuid, &IntentBody),
        owners: &[Uuid],
        node: &Node,
    ) -> Result<Option<IpAddr>, AppError> {
        let now = self.clock.now();
        if !asks(body) {
            tx.release_address(workload, now).await?;
            return Ok(None);
        }
        let pools = self.pools(tx, workload, owners).await?;
        if let Some(address) = pools.held_reaches(node) {
            return Ok(Some(address));
        }
        if pools.held.is_some() {
            tx.release_address(workload, now).await?;
        }
        let (pool, address) = pools.free_on(node, now).ok_or_else(|| {
            AppError::Internal("the chosen node's dedicated address: no pool has one".into())
        })?;
        tx.hold_address((pool, address), workload, now).await?;
        Ok(Some(address))
    }
}
