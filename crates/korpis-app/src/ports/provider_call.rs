// SPDX-License-Identifier: AGPL-3.0-or-later

//! The provider call queue (§3.3 of docs/16-extensions.md): calls are queued by whichever replica
//! wants a provider called, claimed by the replica holding a stream that serves it, and finished
//! by the provider's result, by the stream ending, or by the deadline.

use super::StoreResult;
use korpis_core::extension::{CallOutcome, ProviderCall, ProviderType, Registration};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Reading the queue, and what it is routed by.
pub trait ProviderCallRead {
    /// Which of `organizations` have an installation providing `name` of `kind`.
    fn providing_organizations(
        &self,
        organizations: &[Uuid],
        kind: ProviderType,
        name: &str,
    ) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;

    /// Every registration serving in `organization`, of every type.
    fn registrations_in(
        &self,
        organization: Uuid,
    ) -> impl Future<Output = StoreResult<Vec<Registration>>> + Send;

    /// One call.
    fn provider_call(
        &self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<ProviderCall>>> + Send;
}

/// Writing the queue.
pub trait ProviderCallWrite {
    /// Queues a call.
    fn insert_call(&mut self, call: &ProviderCall) -> impl Future<Output = StoreResult<()>> + Send;

    /// Claims for `registration`, at `at`, as many queued calls for what it serves as it has
    /// slots free, oldest first, if its version is the one registered most recently for what it
    /// serves; nothing otherwise, so a replacement drains the version it replaces. The caller
    /// claims for one registration at a time: the free slots are counted, not locked.
    fn claim_calls(
        &mut self,
        registration: &Registration,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<ProviderCall>>> + Send;

    /// Finishes a call not finished yet, only if it was sent to `registration` when one is
    /// named; whether it finished it.
    fn finish_call(
        &mut self,
        id: Uuid,
        registration: Option<Uuid>,
        outcome: &CallOutcome,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Finishes with `outcome` every call sent to a registration that no longer exists; how many.
    fn fail_calls_without_registration(
        &mut self,
        outcome: &CallOutcome,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Finishes with `outcome` every queued call that no installation provides for any more;
    /// how many.
    fn fail_calls_without_provider(
        &mut self,
        outcome: &CallOutcome,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Deletes every call finished before `before`; how many.
    fn prune_calls(&mut self, before: SystemTime) -> impl Future<Output = StoreResult<u64>> + Send;
}
