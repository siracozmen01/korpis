// SPDX-License-Identifier: AGPL-3.0-or-later

//! Waiting on what the control plane announces rather than asking again: a workload's watch sends
//! its status now and then each change the database announces, so a test that needs a status to
//! come about reads the watch until it has.

use crate::support::Harness;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

/// How long a status may take to come about before the test fails; a bound on a failure, never a
/// pause a passing test waits out.
const BOUND: Duration = Duration::from_secs(60);

/// `workload`'s status, read as root, once `until` holds of it.
pub(crate) async fn status_until(
    h: &Harness,
    workload: &str,
    until: impl Fn(&pb::WorkloadStatus) -> bool,
) -> pb::WorkloadStatus {
    let req = pb::WatchWorkloadRequest { workload: workload.to_owned(), ..Default::default() };
    let mut stream = h.workloads(h.root()).watch_workload(req).await.unwrap();
    let watched = async {
        while let Some(m) = stream.message().await.unwrap() {
            let status = m.to_owned_message().status.into_option().unwrap_or_default();
            if until(&status) {
                return Some(status);
            }
        }
        None
    };
    match tokio::time::timeout(BOUND, watched).await {
        Ok(Some(status)) => status,
        Ok(None) => panic!("the watch of {workload} ended before its status came about"),
        Err(tokio::time::error::Elapsed { .. }) => panic!("{workload}'s status never came about"),
    }
}

/// Every channel the database announces a change on.
const CHANNELS: [&str; 8] = [
    "korpis_workload",
    "korpis_event_written",
    "korpis_endpoint",
    "korpis_certificate",
    "korpis_job",
    "korpis_portal",
    "korpis_provider_call",
    "korpis_event_positioned",
];

/// What `check` finds, asked first and again after each change the database announces: for state
/// no watch carries, such as an assignment, which every change it is made from announces. The
/// listener is open before the first check, so no change between a check and the wait is missed.
pub(crate) async fn changed_until<T, F, Fut>(h: &Harness, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let store = h.db.store().await.unwrap();
    let mut listener = sqlx::postgres::PgListener::connect_with(store.pool()).await.unwrap();
    listener.listen_all(CHANNELS).await.unwrap();
    let found = async {
        loop {
            if let Some(found) = check().await {
                return found;
            }
            listener.recv().await.unwrap();
        }
    };
    tokio::time::timeout(BOUND, found).await.expect("what was waited for never came about")
}

/// A fake's state, announced to whoever waits on it: what a provider a test serves was asked.
pub(crate) struct Shared<T>(std::sync::Arc<tokio::sync::watch::Sender<T>>);

impl<T: Default> Default for Shared<T> {
    fn default() -> Self {
        Self(std::sync::Arc::new(tokio::sync::watch::Sender::new(T::default())))
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(std::sync::Arc::clone(&self.0))
    }
}

impl<T> Shared<T> {
    /// Runs `f` on the state, and wakes whoever waits on it.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut out = None;
        self.0.send_modify(|state| out = Some(f(state)));
        out.expect("send_modify runs its closure")
    }

    /// What `pick` finds in the state, once it finds anything.
    pub(crate) async fn until<R>(&self, pick: impl Fn(&T) -> Option<R>) -> R {
        let mut seen = self.0.subscribe();
        let found = async {
            let state = seen.wait_for(|s| pick(s).is_some()).await.expect("the state is kept");
            pick(&state).expect("found just now")
        };
        tokio::time::timeout(BOUND, found).await.expect("what was waited for never came about")
    }
}
