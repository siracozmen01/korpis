// SPDX-License-Identifier: AGPL-3.0-or-later

//! The change feed: one listening connection for the whole process, holding one connection of the
//! pool however many channels it hears. Workload changes (migration 0010) are fanned out to every
//! subscriber, who also hears a resync on a fixed interval and whenever the listener fell behind
//! or reconnected, because a notification is a wake-up, not a record, and what was missed is not
//! replayed. Events written (migration 0023) wake the event sequencer.

use crate::PgStore;
use korpis_app::ports::{Change, Changes};
use sqlx::postgres::PgListener;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{Instant, Interval, MissedTickBehavior};

/// The channel the workload triggers announce on.
const CHANNEL: &str = "korpis_workload";

/// The channel a transaction that wrote an event announces on, at its commit.
const EVENT_WRITTEN: &str = "korpis_event_written";

/// How many announcements a slow subscriber may fall behind before it is told to resync.
const BACKLOG: usize = 1024;

/// How long the listener waits before reconnecting after losing its connection.
const RECONNECT: Duration = Duration::from_secs(1);

/// The fan-out every subscription reads from.
#[derive(Debug)]
pub(crate) struct Feed {
    sender: broadcast::Sender<Change>,
    resync: Duration,
    /// Woken when an event was written; the sequencer waits on it.
    pub(crate) events_written: tokio::sync::Notify,
}

impl Feed {
    pub(crate) fn new(resync: Duration) -> Arc<Self> {
        let (sender, _) = broadcast::channel(BACKLOG);
        Arc::new(Self { sender, resync, events_written: tokio::sync::Notify::new() })
    }
}

/// One subscriber's view of the feed.
#[derive(Debug)]
pub struct Subscription {
    receiver: broadcast::Receiver<Change>,
    tick: Interval,
}

impl Changes for Subscription {
    async fn next(&mut self) -> Option<Change> {
        tokio::select! {
            received = self.receiver.recv() => match received {
                Ok(change) => Some(change),
                Err(RecvError::Lagged(_)) => Some(Change::Resync),
                Err(RecvError::Closed) => None,
            },
            _ = self.tick.tick() => Some(Change::Resync),
        }
    }
}

impl PgStore {
    /// A subscription to every workload's changes, from now on.
    pub(crate) fn subscription(&self) -> Subscription {
        let period = self.feed.resync;
        let mut tick = tokio::time::interval_at(Instant::now() + period, period);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Subscription { receiver: self.feed.sender.subscribe(), tick }
    }

    /// Starts the one listener that feeds every subscription, for as long as the process runs.
    /// Without it, subscribers still hear the periodic resync, so a watch is late, never wrong.
    pub fn listen(&self) {
        let pool = self.pool.clone();
        let feed = Arc::clone(&self.feed);
        tokio::spawn(async move {
            loop {
                match relay(&pool, &feed).await {
                    Ok(()) => return,
                    Err(e) => tracing::warn!(error = %e, "workload change feed reconnecting"),
                }
                // Whatever was announced while disconnected is lost; everyone re-reads.
                let _ = feed.sender.send(Change::Resync);
                feed.events_written.notify_one();
                tokio::time::sleep(RECONNECT).await;
            }
        });
    }
}

/// Relays announcements until the connection fails.
async fn relay(pool: &sqlx::PgPool, feed: &Feed) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    listener.listen_all([CHANNEL, EVENT_WRITTEN]).await?;
    loop {
        let notification = listener.recv().await?;
        if notification.channel() == EVENT_WRITTEN {
            feed.events_written.notify_one();
            continue;
        }
        let change = notification.payload().parse().map_or(Change::Resync, Change::Workload);
        // No subscriber is not an error: nobody is watching.
        let _ = feed.sender.send(change);
    }
}
