// SPDX-License-Identifier: AGPL-3.0-or-later

//! Providers (§3.3 of docs/16-extensions.md): what installations may serve, and the streams
//! serving it.

use super::StoreResult;
use korpis_core::extension::{ProvidedInterface, Registration};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading providers.
pub trait ProviderRead {
    /// What an installation may serve.
    fn provides_of(
        &self,
        extension: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<ProvidedInterface>>> + Send;

    /// The registrations serving `name` of `kind` in `organization`.
    fn registrations(
        &self,
        organization: Uuid,
        kind: korpis_core::extension::ProviderType,
        name: &str,
    ) -> impl Future<Output = StoreResult<Vec<Registration>>> + Send;
}

/// Writing providers.
pub trait ProviderWrite {
    /// Records what an installation may serve. A name already served by that type in the
    /// organization is [`Unique::ProviderName`](super::Unique::ProviderName).
    fn insert_provides(
        &mut self,
        extension: Uuid,
        organization: Uuid,
        provides: &[ProvidedInterface],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Makes an installation's provides exactly `provides`: entries it no longer has end with
    /// the registrations serving them, entries it keeps keep theirs, and new ones are recorded,
    /// a name already served as in [`ProviderWrite::insert_provides`].
    fn replace_provides(
        &mut self,
        extension: Uuid,
        organization: Uuid,
        provides: &[ProvidedInterface],
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Ends an installation's provides and every registration of its streams.
    fn delete_provides(&mut self, extension: Uuid) -> impl Future<Output = StoreResult<()>> + Send;

    /// A new registration.
    fn insert_registration(
        &mut self,
        registration: &Registration,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Withdraws one; `false` if it was gone already.
    fn delete_registration(&mut self, id: Uuid) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Marks `replica`'s registrations seen at `at`; the identifiers of those that still exist.
    fn touch_registrations(
        &mut self,
        replica: Uuid,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// Withdraws every registration not seen since `before`; those withdrawn.
    fn expire_registrations(
        &mut self,
        before: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Registration>>> + Send;
}
