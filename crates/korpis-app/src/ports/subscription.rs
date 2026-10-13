// SPDX-License-Identifier: AGPL-3.0-or-later

//! Event subscriptions (§5 of docs/15-observability.md): stored positions in the log.

use super::StoreResult;
use crate::Page;
use korpis_core::event::Subscription;
use std::future::Future;
use uuid::Uuid;

/// Reading subscriptions.
pub trait SubscriptionRead {
    /// One subscription.
    fn subscription(
        &self,
        id: Uuid,
    ) -> impl Future<Output = StoreResult<Option<Subscription>>> + Send;

    /// The position of the newest event positioned; 0 before the first.
    fn last_position(&self) -> impl Future<Output = StoreResult<u64>> + Send;

    /// A subject's subscriptions, in identifier order after the page's cursor.
    fn subscriptions_of(
        &self,
        subject_key: &str,
        page: Page,
    ) -> impl Future<Output = StoreResult<Vec<Subscription>>> + Send;
}

/// Writing subscriptions.
pub trait SubscriptionWrite {
    /// A new subscription.
    fn insert_subscription(
        &mut self,
        subscription: &Subscription,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Deletes one; `false` if there was none.
    fn delete_subscription(&mut self, id: Uuid) -> impl Future<Output = StoreResult<bool>> + Send;

    /// Deletes those an installation created; how many.
    fn delete_extension_subscriptions(
        &mut self,
        extension: Uuid,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// Moves the acknowledged position to `through` if that is later; the position after, or
    /// `None` if there is no such subscription.
    fn ack_subscription(
        &mut self,
        id: Uuid,
        through: u64,
    ) -> impl Future<Output = StoreResult<Option<u64>>> + Send;
}
