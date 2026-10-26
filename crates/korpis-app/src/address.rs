// SPDX-License-Identifier: AGPL-3.0-or-later

//! Address pools (§3.7 of docs/07-networking.md, §3.6 of docs/01-model.md): made by an operator
//! vouching for the range, for any organization, deleted by whoever may write them in that
//! organization, and their addresses held on that organization's own nodes alone, since a node
//! holding one translates every connection to it.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::extension::CallTarget;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{
    AddressWrite as _, Clock, EdgeWrite as _, EffectWrite as _, PlacementWrite as _, RecipeSource,
    Store, StoreError, Unique,
};
use korpis_core::address::{AddressPool, DEFAULT_QUARANTINE, Dedicated, Routing, Scope, Taken};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::extension::ProviderType;
use std::net::IpAddr;
use std::time::Duration;
use uuid::Uuid;

mod placed;
mod route;

/// What a pool is made of, as its maker asks for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateAddressPool {
    /// Its organization.
    pub organization: Uuid,
    /// Its addresses.
    pub cidr: ipnet::IpNet,
    /// Where they are held.
    pub scope: Scope,
    /// How they reach their node.
    pub routing: Routing,
    /// The node's interface they are answered for on, where they are on its link.
    pub interface: String,
    /// How long a released address rests; the default where none is asked.
    pub quarantine: Option<Duration>,
    /// Addresses never given.
    pub reserved: Vec<std::net::IpAddr>,
}

/// A pool, with how many of its addresses are held now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressPoolView {
    /// The pool.
    pub pool: AddressPool,
    /// How many of its addresses workloads hold.
    pub held: u64,
}

fn effect(caller: &Caller, action: &str, grant: Uuid, p: &AddressPool) -> Effect {
    let about = ObjectRef::new(Kind::Organization, p.organization);
    caller
        .effect(action, Outcome::Succeeded)
        .under(grant)
        .about(about)
        .after(details([("pool", p.id.to_string().into()), ("cidr", p.cidr.to_string().into())]))
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Refuses a pool that could not be held where it says.
    async fn pool_holdable(&self, p: &AddressPool) -> Result<(), AppError> {
        p.validate().map_err(|e| AppError::invalid("pool", e.to_string()))?;
        if p.cidr.addr().is_ipv6() && p.group().is_none() {
            return Err(AppError::invalid(
                "pool.cidr",
                "a dedicated address is IPv4: a workload's IPv6 address is its own already \
                 (§3.7 of docs/07-networking.md)",
            ));
        }
        match &p.scope {
            Scope::Node(node) => {
                let found = self.store.node(*node).await?;
                if found.is_none_or(|n| n.organization != p.organization) {
                    return Err(AppError::invalid(
                        "pool.node",
                        "a pool's addresses are held on its organization's own nodes alone, \
                         since a node translates every connection to an address it holds",
                    ));
                }
                Ok(())
            }
            Scope::Edge { group, router } => {
                let found = self.store.edge_group(*group).await?;
                if found.is_none_or(|g| g.organization != p.organization) {
                    return Err(AppError::invalid(
                        "pool.group",
                        "an edge pool names a group of its own organization",
                    ));
                }
                match router {
                    Some(r) => self.router_seen(p.organization, r).await,
                    None => Ok(()),
                }
            }
            Scope::Cluster(router) => self.router_seen(p.organization, router).await,
        }
    }

    /// Refuses a router `organization` sees no `address_router` provider of.
    async fn router_seen(&self, organization: Uuid, router: &str) -> Result<(), AppError> {
        let target =
            CallTarget { organization, kind: ProviderType::AddressRouter, name: router.to_owned() };
        if self.provider_owner(&target).await?.is_none() {
            return Err(AppError::invalid(
                "pool.router",
                "no address_router provider of this name is seen by the organization",
            ));
        }
        Ok(())
    }

    /// The dedicated address `workload` holds, as `node` holds it: none where it holds none, or
    /// where the address's pool does not reach the node, which then holds nothing of it.
    pub(crate) async fn dedicated_on(
        &self,
        workload: Uuid,
        node: Uuid,
    ) -> Result<Option<Dedicated>, AppError> {
        let Some(held) = self.store.dedicated_of(workload).await? else { return Ok(None) };
        let (address, pool) = (held.address, held.pool);
        let (Some(pool), Some(n)) =
            (self.store.address_pool(pool).await?, self.store.node(node).await?)
        else {
            return Ok(None);
        };
        let IpAddr::V4(address) = address else {
            return Err(AppError::Internal(format!("dedicated address {address} is not IPv4")));
        };
        // A cluster pool's address is held only on the node its router says it routes it to.
        let routed = !matches!(pool.scope, Scope::Cluster(_)) || held.routed == Some(n.id);
        let reached = routed && pool.reaches(n.id, n.organization);
        Ok(reached.then_some(Dedicated { address, interface: pool.interface }))
    }

    /// Makes an address pool.
    pub async fn create_address_pool(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, AddressPoolView>,
        cmd: CreateAddressPool,
    ) -> Result<Reply<AddressPoolView>, AppError> {
        const OPERATION: &str = "address_pool.create";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        // Nothing in the protocol proves who holds an address range, and pools never overlap, so
        // making one is an operator vouching for it, as `domain.attest` vouches for a name: held
        // on the installation's root organization, for any organization beneath it (Finding 187
        // of docs/23-walkthroughs.md).
        let root = ObjectRef::new(Kind::Organization, self.root_of(cmd.organization).await?);
        let permit = self.authorize(caller, "address_pool.attest", root).await?;
        let pool = AddressPool {
            id: Uuid::now_v7(),
            organization: cmd.organization,
            cidr: cmd.cidr.trunc(),
            scope: cmd.scope,
            routing: cmd.routing,
            interface: cmd.interface,
            quarantine: cmd.quarantine.unwrap_or(DEFAULT_QUARANTINE),
            reserved: cmd.reserved,
            created_at: self.clock.now(),
        };
        self.pool_holdable(&pool).await?;
        let mut tx = self.store.begin().await?;
        tx.insert_address_pool(&pool).await.map_err(|e| match e {
            // Whose it overlaps is not said: it may be another organization's.
            StoreError::Duplicate(Unique::AddressPool) => {
                AppError::conflict("address_pool.overlaps", "it overlaps another pool")
            }
            other => other.into(),
        })?;
        tx.append_effect(&effect(caller, OPERATION, permit.grant, &pool)).await?;
        finish(tx, caller, idem, OPERATION, AddressPoolView { pool, held: 0 }).await
    }

    /// A page of an organization's pools, each with how many of its addresses are held.
    pub async fn list_address_pools(
        &self,
        caller: &Caller,
        organization: Uuid,
        page: Page,
    ) -> Result<Paged<AddressPoolView>, AppError> {
        let org = ObjectRef::new(Kind::Organization, organization);
        self.authorize(caller, "address_pool.read", org).await?;
        let found = self.store.address_pools_of(organization, page).await?;
        let next = page.next(&found, |p| p.id);
        let mut items = Vec::with_capacity(found.len());
        for pool in found {
            let held = self.store.pool_held(pool.id).await?;
            items.push(AddressPoolView { pool, held });
        }
        Ok(Paged { items, next })
    }

    /// Deletes a pool none of whose addresses is held, under the placement lock, so no placement
    /// takes one of them meanwhile.
    pub async fn delete_address_pool(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, ()>,
        id: Uuid,
    ) -> Result<Reply<()>, AppError> {
        const OPERATION: &str = "address_pool.delete";
        if let Some(r) = self.replay(caller, idem, OPERATION).await? {
            return Ok(Reply::Replayed(r));
        }
        let missing = AppError::NotFound { kind: "address pool" };
        let pool = self.store.address_pool(id).await?.ok_or(missing)?;
        let org = ObjectRef::new(Kind::Organization, pool.organization);
        let permit = self.authorize(caller, "address_pool.write", org).await?;
        let mut tx = self.store.begin().await?;
        tx.lock_placement().await?;
        let taken = tx.pool_taken(id).await?;
        let mut held = taken.values().filter(|t| matches!(t, Taken::Held(_))).count();
        held += usize::from(tx.stable_held_in(id).await?.is_some());
        if held > 0 {
            let why = format!("{held} of its addresses are held; retire their workloads first");
            return Err(AppError::conflict("address_pool.held", why));
        }
        tx.delete_address_pool(id).await?;
        tx.append_effect(&effect(caller, OPERATION, permit.grant, &pool)).await?;
        finish(tx, caller, idem, OPERATION, ()).await
    }
}
