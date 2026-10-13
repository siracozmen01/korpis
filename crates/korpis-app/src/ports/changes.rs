// SPDX-License-Identifier: AGPL-3.0-or-later

//! Hearing that a workload changed (§4.3 of docs/02-architecture.md), so a watcher re-reads it
//! when it changes instead of polling it.

use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// Something a watcher should act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// The state this workload's status is read from changed.
    Workload(Uuid),
    /// Re-read everything watched: changes may have been missed (the feed fell behind or
    /// reconnected), or time alone may have changed a status (a scheduled window opening).
    Resync,
}

/// One watcher's subscription to the changes of every workload.
pub trait Changes: Send + 'static {
    /// The next change; `None` once the feed has ended for good.
    fn next(&mut self) -> impl Future<Output = Option<Change>> + Send;
}

/// A wake-up for something that re-reads what it waits on: an event subscriber (new events may
/// have been positioned), or a provider call's caller and dispatcher (a call was queued or
/// finished). Like a change, it is a wake-up and not a record, and it also comes on a fixed
/// interval, so a reader that missed one is late, never stuck.
pub trait Wakeups: Send + 'static {
    /// Waits for the next wake-up; `None` once the feed has ended for good.
    fn next(&mut self) -> impl Future<Output = Option<()>> + Send;

    /// Waits for the next wake-up, or until `deadline` if that comes first.
    fn next_before(&mut self, deadline: SystemTime) -> impl Future<Output = ()> + Send;
}
