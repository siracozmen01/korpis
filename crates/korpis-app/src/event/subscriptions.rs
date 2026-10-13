// SPDX-License-Identifier: AGPL-3.0-or-later

//! Event subscriptions (§5 of docs/15-observability.md). A subscription is its subject's own
//! stored position: creating one needs no particular grant, because what it is delivered is
//! judged by the subject's `event.read` grants when each event is sent, and only its subject may
//! read, acknowledge, or delete it. A new subscription starts at the end of the log, so it is
//! delivered what happens from its creation on.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::idempotency::{Idempotent, Reply, finish};
use crate::page::{Page, Paged};
use crate::ports::{Clock, EffectWrite, RecipeSource, Store, SubscriptionWrite, Tx};
use korpis_core::audit::{Effect, Outcome, details};
use korpis_core::event::{EventFilter, Subscription};
use uuid::Uuid;

const CREATE: &str = "event.subscription.create";
const DELETE: &str = "event.subscription.delete";

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Creates a subscription for the caller.
    pub async fn create_subscription(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, Subscription>,
        filter: EventFilter,
    ) -> Result<Reply<Subscription>, AppError> {
        if let Some(r) = self.replay(caller, idem, CREATE).await? {
            return Ok(Reply::Replayed(r));
        }
        filter.validate().map_err(|e| AppError::invalid("filter.kinds", e.to_string()))?;
        let subscription = Subscription {
            id: Uuid::now_v7(),
            subject: caller.principal.clone(),
            filter,
            acked: self.store.last_position().await?,
            created: self.clock.now(),
            extension: None,
        };
        let mut tx = self.store.begin().await?;
        tx.insert_subscription(&subscription).await?;
        tx.append_effect(&subscription_effect(caller, CREATE, subscription.id)).await?;
        finish(tx, caller, idem, CREATE, subscription).await
    }

    /// One of the caller's subscriptions.
    pub async fn get_subscription(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<Subscription, AppError> {
        self.store
            .subscription(id)
            .await?
            .filter(|s| s.subject == caller.principal)
            .ok_or(AppError::NotFound { kind: "subscription" })
    }

    /// The caller's subscriptions.
    pub async fn list_subscriptions(
        &self,
        caller: &Caller,
        page: Page,
    ) -> Result<Paged<Subscription>, AppError> {
        let items = self.store.subscriptions_of(&caller.principal.key(), page).await?;
        let next = page.next(&items, |s| s.id);
        Ok(Paged { items, next })
    }

    /// Deletes one of the caller's subscriptions.
    pub async fn delete_subscription(
        &self,
        caller: &Caller,
        idem: &Idempotent<'_, ()>,
        id: Uuid,
    ) -> Result<Reply<()>, AppError> {
        if let Some(r) = self.replay(caller, idem, DELETE).await? {
            return Ok(Reply::Replayed(r));
        }
        self.get_subscription(caller, id).await?;
        let mut tx = self.store.begin().await?;
        if !tx.delete_subscription(id).await? {
            return Err(AppError::NotFound { kind: "subscription" });
        }
        tx.append_effect(&subscription_effect(caller, DELETE, id)).await?;
        finish(tx, caller, idem, DELETE, ()).await
    }

    /// Records that the caller has processed one of its subscriptions through position
    /// `through`; an earlier position than already acknowledged changes nothing. Not an effect:
    /// it is a consumer's progress, as frequent as its reads.
    pub async fn ack_subscription(
        &self,
        caller: &Caller,
        id: Uuid,
        through: u64,
    ) -> Result<u64, AppError> {
        self.get_subscription(caller, id).await?;
        if through > self.store.last_position().await? {
            let why = "an event that has been delivered; a later one would skip what is not yet";
            return Err(AppError::invalid("through", why));
        }
        let mut tx = self.store.begin().await?;
        let acked = tx.ack_subscription(id, through).await?;
        tx.commit().await?;
        acked.ok_or(AppError::NotFound { kind: "subscription" })
    }
}

fn subscription_effect(caller: &Caller, action: &str, id: Uuid) -> Effect {
    Effect::new(caller.principal.clone(), action, Outcome::Succeeded)
        .after(details([("subscription", id.to_string().into())]))
}
