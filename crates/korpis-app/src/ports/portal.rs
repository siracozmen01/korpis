// SPDX-License-Identifier: AGPL-3.0-or-later

//! Portals (§3.6 of docs/07-networking.md): names the control plane answers under for an
//! organization.

use super::StoreResult;
use crate::page::Page;
use korpis_core::dns::domain::DomainName;
use korpis_core::dns::portal::Portal;
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading portals.
pub trait PortalRead {
    /// One portal.
    fn portal(&self, id: Uuid) -> impl Future<Output = StoreResult<Option<Portal>>> + Send;

    /// An organization's portals, a page at a time, in identifier order.
    fn portals_of(
        &self,
        organization: Uuid,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Portal>>> + Send;

    /// The portals of any of `names`.
    fn portals_named(
        &self,
        names: &[DomainName],
    ) -> impl Future<Output = StoreResult<Vec<Portal>>> + Send;
}

/// Writing portals. A portal's name is checked against domains and routes under the domains lock
/// ([`DomainWrite::lock_domains`](super::DomainWrite::lock_domains)), which a route's claim takes
/// too.
pub trait PortalWrite {
    /// Every portal of `name` or a name beneath it, as this transaction sees them.
    fn portals_beneath(
        &mut self,
        name: &DomainName,
    ) -> impl Future<Output = StoreResult<Vec<Portal>>> + Send;

    /// The portals of any of `names`, as this transaction sees them.
    fn portals_named_now(
        &mut self,
        names: &[DomainName],
    ) -> impl Future<Output = StoreResult<Vec<Portal>>> + Send;

    /// Makes an `acme` certificate for the portal `portal` of `organization`, for its one name,
    /// pending; its identifier.
    fn acme_portal_certificate(
        &mut self,
        organization: Uuid,
        portal: Uuid,
        name: &DomainName,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<Uuid>> + Send;

    /// Records a portal. A name another portal has is
    /// [`Unique::PortalName`](super::Unique::PortalName).
    fn insert_portal(&mut self, p: &Portal) -> impl Future<Output = StoreResult<()>> + Send;

    /// One portal, locked against change until the transaction ends.
    fn lock_portal(&mut self, id: Uuid)
    -> impl Future<Output = StoreResult<Option<Portal>>> + Send;

    /// Serves the portal `id` with `certificate` from now on.
    fn set_portal_certificate(
        &mut self,
        id: Uuid,
        certificate: Uuid,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// The portals served with the certificate `certificate`, as this transaction sees them.
    fn portals_served_with(
        &mut self,
        certificate: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<Portal>>> + Send;

    /// Removes a portal.
    fn delete_portal(&mut self, id: Uuid) -> impl Future<Output = StoreResult<()>> + Send;
}
