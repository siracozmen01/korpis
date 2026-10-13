// SPDX-License-Identifier: AGPL-3.0-or-later

//! Provider circuit breakers (§5 of docs/16-extensions.md): the decision, shared by every
//! replica, and the evidence it is judged from, the calls the provider last finished.

use super::StoreResult;
use korpis_core::extension::{Breaker, ProviderCondition, ProviderType, Sample};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// A provider, as a breaker is kept for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderKey<'a> {
    /// The organization whose installation serves it.
    pub organization: Uuid,
    /// Its type.
    pub kind: ProviderType,
    /// Its name.
    pub name: &'a str,
}

/// Breakers, in a transaction.
pub trait BreakerWrite {
    /// The provider's breaker, locked until the transaction ends; closed if it has none yet.
    fn lock_breaker(
        &mut self,
        provider: ProviderKey<'_>,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<Breaker>> + Send;

    /// The provider's breaker, unlocked; closed if it has none yet.
    fn read_breaker(
        &mut self,
        provider: ProviderKey<'_>,
    ) -> impl Future<Output = StoreResult<Breaker>> + Send;

    /// Records the provider's breaker.
    fn set_breaker(
        &mut self,
        provider: ProviderKey<'_>,
        breaker: &Breaker,
        at: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// The calls the provider finished since `since`, newest first, at most
    /// [`WINDOW_CALLS`](korpis_core::extension::WINDOW_CALLS), as the breaker judges them.
    fn recent_samples(
        &mut self,
        provider: ProviderKey<'_>,
        since: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Sample>>> + Send;

    /// How many calls are queued for the provider, waiting.
    fn queued_calls(
        &mut self,
        provider: ProviderKey<'_>,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Appends a provider condition event.
    fn append_provider_condition(
        &mut self,
        condition: &ProviderCondition,
    ) -> impl Future<Output = StoreResult<()>> + Send;
}
