// SPDX-License-Identifier: AGPL-3.0-or-later

//! Domains organizations hold (§3.2 of docs/07-networking.md).

use super::StoreResult;
use crate::page::Page;
use korpis_core::dns::domain::{Domain, DomainName, Route};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading domains.
pub trait DomainRead {
    /// One domain.
    fn domain(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Domain>>> + Send;

    /// An organization's domains, a page at a time, in identifier order.
    fn domains_of(
        &self,
        organization: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Domain>>> + Send;

    /// Every verified domain covering `name`, of any organization.
    fn verified_covering(
        &self,
        name: &DomainName,
    ) -> impl Future<Output = StoreResult<Vec<Domain>>> + Send;

    /// The routes of any of `names`.
    fn routes_of(
        &self,
        names: &[DomainName],
    ) -> impl Future<Output = StoreResult<Vec<Route>>> + Send;
}

/// Writing domains.
pub trait DomainWrite {
    /// Serializes every change to which domains are verified, until the transaction ends, so two
    /// that would overlap are never both verified.
    fn lock_domains(&mut self) -> impl Future<Output = StoreResult<()>> + Send;

    /// Every verified domain that covers `name` or lies beneath it, of any organization, as this
    /// transaction sees them.
    fn verified_overlapping(
        &mut self,
        name: &DomainName,
    ) -> impl Future<Output = StoreResult<Vec<Domain>>> + Send;

    /// Records a domain. One an organization already asked for is
    /// [`Unique::DomainName`](super::Unique::DomainName).
    fn insert_domain(&mut self, d: &Domain) -> impl Future<Output = StoreResult<()>> + Send;

    /// Marks a domain verified at `at`.
    fn verify_domain(
        &mut self,
        id: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Removes a domain and every domain delegated beneath it; how many went.
    fn delete_domain(&mut self, id: Uuid) -> impl Future<Output = StoreResult<u64>> + Send;

    /// The domain and every domain delegated beneath it, at any depth.
    fn delegation_tree(&mut self, id: Uuid) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// Every route of `name` or a name beneath it, as this transaction sees them.
    fn routes_beneath(
        &mut self,
        name: &DomainName,
    ) -> impl Future<Output = StoreResult<Vec<Route>>> + Send;

    /// Makes `routes` the workload's, and no other: what it routed before and does not now is
    /// free. Answers the first name another workload routes, if any, having claimed nothing
    /// after it; the caller then abandons the transaction.
    fn claim_routes(
        &mut self,
        workload: Uuid,
        routes: &[Route],
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<Option<DomainName>>> + Send;
}
