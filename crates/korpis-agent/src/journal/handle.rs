// SPDX-License-Identifier: AGPL-3.0-or-later

//! The journal as the rest of the agent shares it. Every call that may touch the disk runs on
//! the blocking pool, since a sync waits on the device; whoever sends the journal is woken
//! whenever its head moves. The handle also measures how fast the journal grows, which is what
//! turns the declared bound into the horizon `NodeStatus` reports (§4.10 of
//! docs/02-architecture.md).

use super::{Appended, Bounds, Journal};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tokio::sync::Notify;

/// How far back the growth rate looks.
const GROWTH_WINDOW: Duration = Duration::from_secs(3600);

/// How full the journal is, and how long that lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    /// The bytes on disk.
    pub used: u64,
    /// The declared bound.
    pub bound: u64,
    /// How long until the bound is reached at the rate measured; `None` while nothing grows.
    pub horizon: Option<Duration>,
}

/// Samples of how much had been appended, and when.
#[derive(Debug, Default)]
struct Growth {
    samples: VecDeque<(SystemTime, u64)>,
}

impl Growth {
    fn sample(&mut self, now: SystemTime, appended: u64) {
        self.samples.push_back((now, appended));
        while self.samples.len() > 2
            && self
                .samples
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t).unwrap_or_default() > GROWTH_WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// How long `free` bytes last at the rate measured.
    fn horizon(&self, free: u64) -> Option<Duration> {
        let ((t0, a0), (t1, a1)) = (self.samples.front()?, self.samples.back()?);
        let grown = u128::from(a1.saturating_sub(*a0));
        let elapsed = t1.duration_since(*t0).ok()?.as_millis();
        if grown == 0 || elapsed == 0 {
            return None;
        }
        let ms = u128::from(free).saturating_mul(elapsed) / grown;
        Some(Duration::from_millis(u64::try_from(ms).unwrap_or(u64::MAX)))
    }
}

#[derive(Debug)]
struct Inner {
    journal: Journal,
    growth: Growth,
}

/// The shared journal.
#[derive(Debug, Clone)]
pub struct JournalHandle {
    inner: Arc<Mutex<Inner>>,
    changed: Arc<Notify>,
}

fn poisoned<T>(_: T) -> std::io::Error {
    std::io::Error::other("the journal's lock is poisoned")
}

impl JournalHandle {
    /// Opens the journal kept in `dir`.
    pub fn open(dir: &Path, bounds: Bounds) -> std::io::Result<Self> {
        let journal = Journal::open(dir, bounds)?;
        let inner = Inner { journal, growth: Growth::default() };
        Ok(Self { inner: Arc::new(Mutex::new(inner)), changed: Arc::new(Notify::new()) })
    }

    /// Runs `f` on the journal on the blocking pool, waking the sender if the head moved.
    async fn with<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Inner) -> std::io::Result<T> + Send + 'static,
    ) -> std::io::Result<T> {
        let (inner, changed) = (Arc::clone(&self.inner), Arc::clone(&self.changed));
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock().map_err(poisoned)?;
            let head = guard.journal.head();
            let out = f(&mut guard);
            if guard.journal.head() != head {
                changed.notify_one();
            }
            out
        })
        .await
        .map_err(std::io::Error::other)?
    }

    /// Appends `body`, happening now.
    pub async fn append(&self, body: Body) -> std::io::Result<Appended> {
        self.with(move |i| i.journal.append(&body, SystemTime::now())).await
    }

    /// The control plane has ingested everything through `seq`.
    pub async fn acknowledge(&self, seq: u64) -> std::io::Result<()> {
        self.with(move |i| i.journal.acknowledge(seq, SystemTime::now())).await
    }

    /// The records after `after`, up to about `max_bytes`.
    pub async fn read_after(
        &self,
        after: u64,
        max_bytes: u64,
    ) -> std::io::Result<Vec<agent::JournalRecord>> {
        self.with(move |i| i.journal.read_after(after, max_bytes)).await
    }

    /// The highest number written and the lowest held, as the Hello reports them.
    pub async fn marks(&self) -> std::io::Result<(u64, u64)> {
        self.with(|i| Ok((i.journal.head(), i.journal.tail()))).await
    }

    /// How full the journal is, sampling its growth.
    pub async fn usage(&self) -> std::io::Result<Usage> {
        self.with(|i| {
            let (used, bound) = (i.journal.used(), i.journal.bounds.bytes);
            i.growth.sample(SystemTime::now(), i.journal.appended);
            let horizon = i.growth.horizon(bound.saturating_sub(used));
            Ok(Usage { used, bound, horizon })
        })
        .await
    }

    /// Writes repeats that are due, discards what retention allows, and writes a pending gap.
    pub async fn maintain(&self) -> std::io::Result<()> {
        self.with(|i| i.journal.maintain(SystemTime::now())).await
    }

    /// Maintains the journal every `period`, connected or not, for as long as the process runs.
    #[must_use = "the task stops when the handle is aborted"]
    pub fn maintain_every(&self, period: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(maintain_loop(self.clone(), period))
    }

    /// Writes every repeat still counted: for a clean shutdown.
    pub async fn close(&self) -> std::io::Result<()> {
        self.with(|i| i.journal.close(SystemTime::now())).await
    }

    /// Waits until the head moves.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }
}

async fn maintain_loop(handle: JournalHandle, period: Duration) {
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Err(e) = handle.maintain().await {
            tracing::error!(error = %e, "journal maintenance failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_horizon_is_the_free_space_at_the_rate_measured() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
        let mut g = Growth::default();
        g.sample(t, 0);
        assert_eq!(g.horizon(1000), None, "one sample measures nothing");
        g.sample(t + Duration::from_secs(10), 100);
        assert_eq!(g.horizon(1000), Some(Duration::from_secs(100)));
        g.sample(t + Duration::from_secs(20), 100);
        assert_eq!(g.horizon(1000), Some(Duration::from_secs(200)), "the rate halved");
    }
}
