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
        Err(_) => panic!("{workload}'s status never came about"),
    }
}

/// Every channel the database announces a change on.
const CHANNELS: [&str; 7] = [
    "korpis_workload",
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
