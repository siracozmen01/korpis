// SPDX-License-Identifier: AGPL-3.0-or-later

//! `ingress` through an edge group (§3.1 of docs/07-networking.md), decided where a workload is
//! placed: the group serving its `stable` endpoints where that one can, otherwise the group
//! serving its `ingress` already where that still can, otherwise the nearest organization's
//! group that can. A group can where two of its members or more serve the ingress, running the
//! proxy on ports 80 and 443 of their own, and it has an IPv4 ingress address or a pool with room
//! for one: an address whose ports 80 and 443, over TCP and UDP, no `stable` endpoint holds,
//! chosen at the first placement that needs it and kept with its pool, so the names that resolve
//! to it do not move. An IPv6 one is chosen beside it where an IPv6 pool of the group has room.
//! The address is held as any of the group's, under a lease, by a member serving the ingress.

use super::placed::Serving;
use crate::App;
use crate::error::AppError;
use crate::ports::{
    AddressWrite as _, Clock, EdgeIngressWrite as _, EdgeWrite as _, RecipeSource, Store, Tx as _,
};
use korpis_core::address::{AddressPool, Taken};
use korpis_core::edge::{EdgeGroup, IngressAddress, StablePort, ingress_room};
use korpis_core::workload::spec::IntentBody;
use korpis_core::workload::spec::endpoints::Exposure;
use uuid::Uuid;

/// The group serving a workload's `ingress` endpoints, with its ingress addresses: those it has,
/// and those this placement chooses.
#[derive(Debug, Clone)]
pub(crate) struct IngressServing {
    /// The group.
    pub(crate) group: EdgeGroup,
    /// Its ingress addresses.
    pub(crate) addresses: Vec<IngressAddress>,
}

/// Whether `body` declares an `ingress` endpoint routing a name.
pub(crate) fn declares_ingress(body: &IntentBody) -> bool {
    body.endpoints.iter().any(|e| e.exposure == Exposure::Ingress && !e.domains.is_empty())
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// The nodes of `members` that serve the ingress, holding ports 80 and 443 for its proxy.
    pub(crate) async fn proxying(&self, members: &[Uuid]) -> Result<Vec<Uuid>, AppError> {
        let mut out = Vec::new();
        for m in members {
            let node = self.store.node(*m).await?;
            if node.and_then(|n| n.advertised).is_some_and(|a| a.ingress) {
                out.push(*m);
            }
        }
        Ok(out)
    }

    /// `g` with only the members of it that serve the ingress, which alone hold its ingress
    /// addresses.
    pub(crate) async fn proxying_group(&self, g: &EdgeGroup) -> Result<EdgeGroup, AppError> {
        Ok(EdgeGroup { members: self.proxying(&g.members).await?, ..g.clone() })
    }

    /// The ingress addresses `g` has, and one chosen of each family it lacks where one of its
    /// pools of that family has room beside what `allotted` gives this placement's `stable`
    /// endpoints; none where it has no IPv4 one and can have none.
    async fn ingress_addresses(
        &self,
        tx: &mut S::Tx,
        (g, owners): (&EdgeGroup, &[Uuid]),
        allotted: &[StablePort],
    ) -> Result<Option<Vec<IngressAddress>>, AppError> {
        let mut out = tx.ingress_addresses_now(g.id).await?;
        let mut pools: Vec<AddressPool> = Vec::new();
        for org in owners {
            let all = tx.address_pools_now(*org).await?;
            pools.extend(all.into_iter().filter(|p| p.group() == Some(g.id)));
        }
        for v4 in [true, false] {
            if out.iter().any(|a| a.address.is_ipv4() == v4) {
                continue;
            }
            let family: Vec<&AddressPool> =
                pools.iter().filter(|p| p.cidr.addr().is_ipv4() == v4).collect();
            out.extend(self.ingress_room_in(tx, (g, &family), allotted).await?);
        }
        Ok(out.iter().any(|a| a.address.is_ipv4()).then_some(out))
    }

    /// The first of `pools`' addresses with room for `g`'s ingress, beside what `allotted` gives
    /// this placement's `stable` endpoints.
    async fn ingress_room_in(
        &self,
        tx: &mut S::Tx,
        (g, pools): (&EdgeGroup, &[&AddressPool]),
        allotted: &[StablePort],
    ) -> Result<Option<IngressAddress>, AppError> {
        let now = self.clock.now();
        for p in pools {
            let mut taken = tx.stable_taken(p.id).await?;
            let beside = allotted.iter().filter(|s| s.pool == p.id);
            taken.extend(
                beside.map(|s| ((s.address, s.port, s.transport), Taken::Held(s.workload))),
            );
            if let Some(address) = ingress_room((p, &taken), now) {
                return Ok(Some(IngressAddress { address, group: g.id, pool: p.id }));
            }
        }
        Ok(None)
    }

    /// The group serving `workload`'s `ingress` endpoints as `body` declares them, and its
    /// ingress addresses: `stable`'s where it can, the one serving them already where it still
    /// can, and otherwise the first group of the nearest of `owners` that can. None where `body`
    /// declares none, or no group can; the node's own proxy serves them then.
    pub(crate) async fn ingress_serving(
        &self,
        tx: &mut S::Tx,
        (workload, body): (Uuid, &IntentBody),
        owners: &[Uuid],
        stable: Option<&Serving>,
    ) -> Result<Option<IngressServing>, AppError> {
        if !declares_ingress(body) {
            return Ok(None);
        }
        let serving = self.store.ingress_served_of(workload).await?;
        let first = stable.map(|s| s.group.id);
        let mut groups = tx.edge_groups_now(owners).await?;
        let nearness = |g: &EdgeGroup| owners.iter().position(|o| *o == g.organization);
        groups.sort_by_key(|g| (Some(g.id) != first, Some(g.id) != serving, nearness(g), g.id));
        for g in groups.into_iter().filter(EdgeGroup::serves) {
            if self.proxying(&g.members).await?.len() < 2 {
                continue;
            }
            let allotted = stable.map_or(&[][..], |s| s.ports.as_slice());
            if let Some(addresses) = self.ingress_addresses(tx, (&g, owners), allotted).await? {
                return Ok(Some(IngressServing { group: g, addresses }));
            }
        }
        Ok(None)
    }

    /// Records what `serving` gives `workload`: the group's ingress addresses chosen now, and
    /// the group, or that none serves it.
    pub(crate) async fn hold_ingress(
        &self,
        tx: &mut S::Tx,
        workload: Uuid,
        serving: Option<&IngressServing>,
    ) -> Result<(), AppError> {
        let now = self.clock.now();
        let Some(s) = serving else {
            tx.serve_ingress(workload, None, now).await?;
            return Ok(());
        };
        let had = tx.ingress_addresses_now(s.group.id).await?;
        for a in s.addresses.iter().filter(|a| !had.contains(a)) {
            tx.hold_ingress_address(a, now).await?;
        }
        tx.serve_ingress(workload, Some(s.group.id), now).await?;
        Ok(())
    }

    /// Records what `node`'s proxy holds of the challenges of its groups' names, which the
    /// issuer waits on before a directory is told to look (§3.1 of docs/07-networking.md).
    ///
    /// # Errors
    /// The store refused it.
    pub async fn record_edge_challenges(
        &self,
        node: Uuid,
        held: &crate::ports::ChallengesHeld,
    ) -> Result<(), AppError> {
        let mut tx = self.store.begin().await?;
        tx.record_edge_challenges(node, held, self.clock.now()).await?;
        tx.commit().await?;
        Ok(())
    }
}
