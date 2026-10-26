// SPDX-License-Identifier: AGPL-3.0-or-later

//! The reconciler of the routes of cluster pools' addresses (§3.7 of docs/07-networking.md):
//! whenever an endpoint is placed, moved, or removed, which is when a workload holding such an
//! address moves or goes, and every thirty seconds besides, it asks each address's router to
//! route it to the node its holder is placed on, or nowhere, a few at once. Each address is taken
//! under a hold, so replicas share the work rather than repeat it.

use crate::api::Api;
use korpis_app::ports::{Store as _, Wakeups as _};
use std::sync::Arc;

/// How many routers are asked at once.
const AT_ONCE: usize = 4;

/// Asks until nothing is due, `AT_ONCE` addresses at a time.
async fn sweep(api: &Arc<Api>) {
    loop {
        let mut asked = tokio::task::JoinSet::new();
        for _ in 0..AT_ONCE {
            let api = Arc::clone(api);
            asked.spawn(async move { api.app().route_next_address().await });
        }
        let mut routed = false;
        while let Some(done) = asked.join_next().await {
            match done {
                Ok(Ok(one)) => routed |= one,
                Ok(Err(e)) => tracing::warn!(error = %e, "an address's router was not asked"),
                Err(e) => tracing::error!(error = %e, "asking a router ended abnormally"),
            }
        }
        if !routed {
            return;
        }
    }
}

/// Runs the reconciler; the task ends with the process.
pub async fn keep(api: Arc<Api>) {
    let mut wakeups = api.app().store().endpoint_changes();
    loop {
        sweep(&api).await;
        if wakeups.next().await.is_none() {
            return;
        }
    }
}
