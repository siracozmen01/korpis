// SPDX-License-Identifier: AGPL-3.0-or-later

//! Waiting on what the control plane's database announces rather than asking again.

use super::ControlPlane;
use std::time::Duration;

/// How long what is waited for may take before the test fails; a bound on a failure, never a
/// pause a passing test waits out.
const BOUND: Duration = Duration::from_secs(120);

impl ControlPlane {
    /// What `check` finds, asked first and again after each change to a workload the database
    /// announces: its placement, plan, power, observation or migration. The listener is open
    /// before the first check, so no change between a check and the wait is missed.
    pub async fn announced<T, F, Fut>(&self, mut check: F) -> T
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Option<T>>,
    {
        let store = self.store().await;
        let mut listener = sqlx::postgres::PgListener::connect_with(store.pool()).await.unwrap();
        listener.listen("korpis_workload").await.unwrap();
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
}
