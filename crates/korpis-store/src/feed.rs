// SPDX-License-Identifier: AGPL-3.0-or-later

//! The change feed: one listening connection for the whole process, holding one connection of the
//! pool however many channels it hears. Workload changes (migration 0010) are fanned out to every
//! subscriber, who also hears a resync on a fixed interval and whenever the listener fell behind
//! or reconnected, because a notification is a wake-up, not a record, and what was missed is not
//! replayed. Events written (migration 0023) wake the event sequencer, and provider calls queued
//! or finished (migration 0027) wake their callers and the dispatchers.

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

/// The channel the sequencer announces newly positioned events on.
const EVENT_POSITIONED: &str = "korpis_event_positioned";

/// The channel a provider call announces on when it is queued and when it finishes.
const PROVIDER_CALL: &str = "korpis_provider_call";

/// The channel a job announces on when it is queued or put back for a later attempt.
const JOB: &str = "korpis_job";

/// The channel a new `acme` certificate announces on.
const CERTIFICATE: &str = "korpis_certificate";

/// How often the issuer looks for certificates due without a wake-up: a retry's or a renewal's
/// time comes by the clock, not by an announcement.
const CERTIFICATE_TICK: Duration = Duration::from_secs(10);

/// The channel an endpoint placed, moved, or removed, or a node readdressed, announces on.
const ENDPOINT: &str = "korpis_endpoint";

/// How often the reconciler of endpoint records looks without a wake-up: a retry's time comes by
/// the clock.
const ENDPOINT_TICK: Duration = Duration::from_secs(30);

/// How often a worker looks for due jobs without a wake-up: a retry's time comes by the clock,
/// not by an announcement, and a missed announcement costs at most this.
const JOB_TICK: Duration = Duration::from_secs(5);

/// How often a provider call's waiter re-reads without a wake-up: calls are short-lived, so a
/// missed announcement costs at most this.
const CALL_TICK: Duration = Duration::from_secs(1);

/// The channel a portal made, changed, or deleted, or its certificate changed, announces on.
const PORTAL: &str = "korpis_portal";

/// How often a replica reads the portals it serves without a wake-up.
const PORTAL_TICK: Duration = Duration::from_secs(60);

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
    /// Bumped whenever events are positioned; event subscribers wait on it.
    positioned: tokio::sync::watch::Sender<u64>,
    /// Bumped whenever a provider call is queued or finished.
    calls: tokio::sync::watch::Sender<u64>,
    /// Bumped whenever a job is queued or put back.
    jobs: tokio::sync::watch::Sender<u64>,
    /// Bumped whenever an `acme` certificate is made.
    certificates: tokio::sync::watch::Sender<u64>,
    /// Bumped whenever an endpoint or a node's addresses change.
    endpoints: tokio::sync::watch::Sender<u64>,
    /// Bumped whenever a portal or its certificate changes.
    portals: tokio::sync::watch::Sender<u64>,
}

impl Feed {
    pub(crate) fn new(resync: Duration) -> Arc<Self> {
        let (sender, _) = broadcast::channel(BACKLOG);
        let (positioned, _) = tokio::sync::watch::channel(0);
        let (calls, _) = tokio::sync::watch::channel(0);
        let (jobs, _) = tokio::sync::watch::channel(0);
        let (certificates, _) = tokio::sync::watch::channel(0);
        let (endpoints, _) = tokio::sync::watch::channel(0);
        let (portals, _) = tokio::sync::watch::channel(0);
        let events_written = tokio::sync::Notify::new();
        Arc::new(Self {
            sender,
            resync,
            events_written,
            positioned,
            calls,
            jobs,
            certificates,
            endpoints,
            portals,
        })
    }
}

/// One waiter's wake-ups: an event subscriber's, or a provider call's.
#[derive(Debug)]
pub struct Wakeups {
    receiver: tokio::sync::watch::Receiver<u64>,
    tick: Interval,
}

impl korpis_app::ports::Wakeups for Wakeups {
    async fn next(&mut self) -> Option<()> {
        tokio::select! {
            changed = self.receiver.changed() => changed.ok(),
            _ = self.tick.tick() => Some(()),
        }
    }

    async fn next_before(&mut self, deadline: std::time::SystemTime) {
        let left = deadline.duration_since(std::time::SystemTime::now()).unwrap_or_default();
        let _ = tokio::time::timeout(left, self.next()).await;
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

    /// Wake-ups for one event subscriber, from now on.
    pub(crate) fn event_positions(&self) -> Wakeups {
        let period = self.feed.resync;
        let mut tick = tokio::time::interval_at(Instant::now() + period, period);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Wakeups { receiver: self.feed.positioned.subscribe(), tick }
    }

    /// Wake-ups for one provider call's caller or dispatcher, from now on.
    pub(crate) fn provider_call_wakeups(&self) -> Wakeups {
        let mut tick = tokio::time::interval_at(Instant::now() + CALL_TICK, CALL_TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Wakeups { receiver: self.feed.calls.subscribe(), tick }
    }

    /// Wake-ups for one job worker, from now on.
    pub(crate) fn job_wakeups(&self) -> Wakeups {
        let mut tick = tokio::time::interval_at(Instant::now() + JOB_TICK, JOB_TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Wakeups { receiver: self.feed.jobs.subscribe(), tick }
    }

    /// Wake-ups for the issuer of certificates, from now on.
    pub(crate) fn certificate_wakeups(&self) -> Wakeups {
        let start = Instant::now() + CERTIFICATE_TICK;
        let mut tick = tokio::time::interval_at(start, CERTIFICATE_TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Wakeups { receiver: self.feed.certificates.subscribe(), tick }
    }

    /// Wake-ups for a replica's listener, which serves the portals, from now on.
    pub(crate) fn portal_wakeups(&self) -> Wakeups {
        let mut tick = tokio::time::interval_at(Instant::now() + PORTAL_TICK, PORTAL_TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Wakeups { receiver: self.feed.portals.subscribe(), tick }
    }

    /// Wake-ups for the reconciler of endpoint records, from now on.
    pub(crate) fn endpoint_wakeups(&self) -> Wakeups {
        let mut tick = tokio::time::interval_at(Instant::now() + ENDPOINT_TICK, ENDPOINT_TICK);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Wakeups { receiver: self.feed.endpoints.subscribe(), tick }
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
                feed.positioned.send_modify(|p| *p += 1);
                feed.calls.send_modify(|c| *c += 1);
                feed.jobs.send_modify(|j| *j += 1);
                feed.certificates.send_modify(|c| *c += 1);
                feed.endpoints.send_modify(|e| *e += 1);
                feed.portals.send_modify(|p| *p += 1);
                tokio::time::sleep(RECONNECT).await;
            }
        });
    }
}

/// Relays announcements until the connection fails.
async fn relay(pool: &sqlx::PgPool, feed: &Feed) -> Result<(), sqlx::Error> {
    let mut listener = PgListener::connect_with(pool).await?;
    let channels = [
        CHANNEL,
        EVENT_WRITTEN,
        EVENT_POSITIONED,
        PROVIDER_CALL,
        JOB,
        CERTIFICATE,
        ENDPOINT,
        PORTAL,
    ];
    listener.listen_all(channels).await?;
    loop {
        let notification = listener.recv().await?;
        match notification.channel() {
            EVENT_WRITTEN => {
                feed.events_written.notify_one();
                continue;
            }
            PROVIDER_CALL => {
                feed.calls.send_modify(|c| *c += 1);
                continue;
            }
            JOB => {
                feed.jobs.send_modify(|j| *j += 1);
                continue;
            }
            CERTIFICATE => {
                feed.certificates.send_modify(|c| *c += 1);
                continue;
            }
            ENDPOINT => {
                feed.endpoints.send_modify(|e| *e += 1);
                continue;
            }
            PORTAL => {
                feed.portals.send_modify(|p| *p += 1);
                continue;
            }
            EVENT_POSITIONED => {
                let last = notification.payload().parse().unwrap_or(0);
                feed.positioned.send_modify(|p| *p = last.max(*p + 1));
                continue;
            }
            _ => {}
        }
        let change = notification.payload().parse().map_or(Change::Resync, Change::Workload);
        // No subscriber is not an error: nobody is watching.
        let _ = feed.sender.send(change);
    }
}
