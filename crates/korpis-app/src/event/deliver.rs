// SPDX-License-Identifier: AGPL-3.0-or-later

//! Delivering a subscription (§5 of docs/15-observability.md): the events after its
//! acknowledged position, in position order, that pass its filter and that its subject may
//! read, judged by the subject's grants as they are when each batch is sent; then each new one
//! as it is positioned. Delivery reads the log at the subscriber's pace, so a slow subscriber
//! falls behind in the log rather than in a buffer, and the bound is the log's retention: a
//! position that fell out of it is answered with a gap marker naming where delivery resumes.

use crate::App;
use crate::caller::Caller;
use crate::error::AppError;
use crate::ports::{Clock, RecipeSource, Store, Wakeups};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::event::Event;
use uuid::Uuid;

/// The most events read from the log at once.
const BATCH: u32 = 200;

/// A delivery in progress.
#[derive(Debug)]
pub struct Delivery<P> {
    subscription: Uuid,
    cursor: u64,
    positions: P,
}

/// What a delivery hands over.
#[derive(Debug, Clone, PartialEq)]
pub enum Delivered {
    /// An event.
    Event(Event),
    /// Events the subscription lost to retention; delivery continues at `resumes_at`.
    Gap {
        /// The oldest event the log still keeps.
        resumes_at: u64,
    },
}

/// The object an event is read under: its workload, or else its organization.
fn object(e: &Event) -> ObjectRef {
    match e.workload {
        Some(w) => ObjectRef::new(Kind::Workload, w),
        None => ObjectRef::new(Kind::Organization, e.organization),
    }
}

impl<S: Store, C: Clock, R: RecipeSource> App<S, C, R> {
    /// Starts delivering one of the caller's subscriptions after its acknowledged position. The
    /// wake-ups are taken before the position is read, so nothing positioned between is missed.
    pub async fn subscribe(
        &self,
        caller: &Caller,
        id: Uuid,
    ) -> Result<Delivery<S::Wakeups>, AppError> {
        let positions = self.store.positions();
        let subscription = self.get_subscription(caller, id).await?;
        Ok(Delivery { subscription: id, cursor: subscription.acked, positions })
    }

    /// The next batch to deliver, waiting until there is one; `None` once the feed has ended.
    /// A subscription deleted meanwhile ends the delivery as not found.
    pub async fn next_delivery(
        &self,
        caller: &Caller,
        d: &mut Delivery<S::Wakeups>,
    ) -> Result<Option<Vec<Delivered>>, AppError> {
        loop {
            let subscription = self.get_subscription(caller, d.subscription).await?;
            let mut out = Vec::new();
            // With nothing kept, the oldest event is the next one to be positioned.
            let first = self.store.first_position().await?;
            if first > d.cursor + 1 {
                out.push(Delivered::Gap { resumes_at: first });
                d.cursor = first - 1;
            }
            let events = self.store.events_after(d.cursor, BATCH).await?;
            if let Some(last) = events.last() {
                d.cursor = last.position;
            }
            let wanted: Vec<Event> =
                events.into_iter().filter(|e| subscription.filter.matches(e)).collect();
            let objects: Vec<ObjectRef> = wanted.iter().map(object).collect();
            let readable = self.permitted_each(caller, "event.read", &objects).await?;
            out.extend(
                wanted
                    .into_iter()
                    .zip(readable)
                    .filter(|(_, r)| *r)
                    .map(|(e, _)| Delivered::Event(e)),
            );
            if !out.is_empty() {
                return Ok(Some(out));
            }
            if d.cursor == self.store.last_position().await? && d.positions.next().await.is_none() {
                return Ok(None);
            }
        }
    }
}
