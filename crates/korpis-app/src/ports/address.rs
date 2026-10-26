// SPDX-License-Identifier: AGPL-3.0-or-later

//! Address pools and the dedicated addresses given from them (§3.7 of docs/07-networking.md).

use super::StoreResult;
use crate::page::Page;
use korpis_core::address::{AddressPool, DueRoute, Holding, Routed, Taken};
use std::collections::BTreeMap;
use std::future::Future;
use std::net::IpAddr;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading address pools.
pub trait AddressRead {
    /// One pool.
    fn address_pool(
        &self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<AddressPool>>> + Send;

    /// A page of an organization's pools, in identifier order.
    fn address_pools_of(
        &self,
        organization: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<AddressPool>>> + Send;

    /// The address `workload` holds, if it holds one.
    fn dedicated_of(
        &self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Holding>>> + Send;

    /// How many of `pool`'s addresses are held now.
    fn pool_held(&self, pool: Uuid) -> impl Future<Output = StoreResult<u64>> + Send;
}

/// Writing address pools and giving their addresses, which placement does under its lock.
pub trait AddressWrite {
    /// Records a pool. One overlapping another is
    /// [`Unique::AddressPool`](super::Unique::AddressPool).
    fn insert_address_pool(
        &mut self,
        pool: &AddressPool,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Deletes a pool, and every address of it resting; whether there was one.
    fn delete_address_pool(&mut self, id: Uuid) -> impl Future<Output = StoreResult<bool>> + Send;

    /// The pools of `organization`, as this transaction sees them.
    fn address_pools_now(
        &mut self,
        organization: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<AddressPool>>> + Send;

    /// What holds each address of `pool` that has a row, as this transaction sees it.
    fn pool_taken(
        &mut self,
        pool: Uuid,
    ) -> impl Future<Output = StoreResult<BTreeMap<IpAddr, Taken>>> + Send;

    /// The address `workload` holds, with its pool, as this transaction sees it.
    fn dedicated_now(
        &mut self,
        workload: Uuid,
    ) -> impl Future<Output = StoreResult<Option<(IpAddr, Uuid)>>> + Send;

    /// Gives `address` of `pool` to `workload`, over a row of it whose rest has ended.
    fn hold_address(
        &mut self,
        held: (Uuid, IpAddr),
        workload: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Takes back the address `workload` holds, to rest from `at`; whether it held one.
    fn release_address(
        &mut self,
        workload: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;
}

/// Asking a cluster pool's router to route its addresses (§3.7 of docs/07-networking.md).
pub trait AddressRoute {
    /// Takes the first address due: one whose holder is on another node than its router last
    /// said it routes it to, or that nothing holds and is still routed, whose backoff has passed
    /// and that no control plane holds; held until `until`. A cluster pool's address is held by
    /// its workload where it is placed, a routed edge pool's by its lease's member; the cluster
    /// pools' are taken first.
    fn take_due_route(
        &mut self,
        now: SystemTime,
        until: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<DueRoute>>> + Send;

    /// Records that the router routes `address`, held as it says, to `node`, or nowhere, and
    /// lets it go.
    fn route_answered(
        &mut self,
        address: (Routed, IpAddr),
        node: Option<Uuid>,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Records the router's failure to route `address`, held as it says, with its reason and
    /// how many calls have failed in a row, to be asked again at `due`, and lets it go.
    fn route_failed(
        &mut self,
        address: (Routed, IpAddr),
        failure: (&str, u32),
        due: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
