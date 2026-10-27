// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whether a workload's `stable` endpoints, and its `ingress` endpoints an edge group serves, are
//! known to be reached (§3.1 of docs/07-networking.md, Finding 190 of docs/23-walkthroughs.md):
//! each with the edge group its address is of, and why it is not reached where the control plane
//! knows it is not. The
//! control plane, which grants the address and probes the group, is what knows; the workload's
//! node cannot see the edge.

use crate::App;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store};
use korpis_core::edge::lease::unreached;
use korpis_core::edge::{IngressAddress, StablePort};
use std::collections::BTreeMap;
use uuid::Uuid;

/// A `stable` or group-served `ingress` endpoint's group, and why it is not reached; none where
/// nothing says it is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeReach {
    /// The edge group its address is of.
    pub group: Uuid,
    /// Why it is not reached now.
    pub unreached: Option<&'static str>,
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Each endpoint of `stable`, by name, with its group and why it is not reached.
    pub(crate) async fn edge_reach(
        &self,
        stable: &[StablePort],
    ) -> Result<Vec<(String, EdgeReach)>, AppError> {
        if stable.is_empty() {
            return Ok(Vec::new());
        }
        let now = self.clock.now();
        let members = self.store.edge_members_all().await?;
        let mut groups: BTreeMap<Uuid, Option<Uuid>> = BTreeMap::new();
        let mut out: Vec<(String, EdgeReach)> = Vec::new();
        for port in stable {
            if out.iter().any(|(name, _)| *name == port.endpoint) {
                continue;
            }
            if let std::collections::btree_map::Entry::Vacant(v) = groups.entry(port.pool) {
                let pool = self.store.address_pool(port.pool).await?;
                v.insert(pool.and_then(|p| p.group()));
            }
            let group = groups.get(&port.pool).copied().flatten();
            let Some(group) = group else { continue };
            let states: Vec<_> = members.iter().filter(|m| m.group == group).cloned().collect();
            let addresses = self.store.edge_addresses_of(group).await?;
            let why = unreached((&states, &addresses), port.address, now);
            out.push((port.endpoint.clone(), EdgeReach { group, unreached: why }));
        }
        Ok(out)
    }

    /// Each of the `ingress` endpoints named `names` that group `group` serves at `addresses`,
    /// its ingress addresses, with why it is not reached: as a `stable` endpoint at its IPv4
    /// ingress address, or because the group has none.
    pub(crate) async fn ingress_reach(
        &self,
        group: Uuid,
        names: &[String],
        addresses: &[IngressAddress],
    ) -> Result<Vec<(String, EdgeReach)>, AppError> {
        let states: Vec<_> =
            self.store.edge_members_all().await?.into_iter().filter(|m| m.group == group).collect();
        let held = self.store.edge_addresses_of(group).await?;
        let v4 = addresses.iter().find(|a| a.address.is_ipv4());
        let why = match v4 {
            Some(a) => unreached((&states, &held), a.address, self.clock.now()),
            None => Some("its edge group has no ingress address"),
        };
        Ok(names.iter().map(|n| (n.clone(), EdgeReach { group, unreached: why })).collect())
    }
}
