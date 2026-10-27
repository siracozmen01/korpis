// SPDX-License-Identifier: AGPL-3.0-or-later

//! The reconciler of block classes' devices (§2.1 of docs/06-storage.md): every few seconds it
//! wants a device for each volume of a block class that a placed workload declares, then asks
//! each device's provider for what it is due, made or attached at its workload's lease, a few at
//! once. Each device is taken under a hold, so replicas share the work rather than repeat it.

use crate::api::Api;
use std::sync::Arc;
use std::time::Duration;

/// How many providers are asked at once.
const AT_ONCE: usize = 4;

/// How often devices are looked at: a lease granted is answered with an attach within this.
const EVERY: Duration = Duration::from_secs(2);

/// Asks until nothing is due, `AT_ONCE` devices at a time.
async fn sweep(api: &Arc<Api>) {
    loop {
        let mut asked = tokio::task::JoinSet::new();
        for _ in 0..AT_ONCE {
            let api = Arc::clone(api);
            asked.spawn(async move { api.app().call_next_device().await });
        }
        let mut called = false;
        while let Some(done) = asked.join_next().await {
            match done {
                Ok(Ok(one)) => called |= one,
                Ok(Err(e)) => tracing::warn!(error = %e, "a device's provider was not asked"),
                Err(e) => tracing::error!(error = %e, "asking a provider ended abnormally"),
            }
        }
        if !called {
            return;
        }
    }
}

/// Runs the reconciler; the task ends with the process.
pub async fn keep(api: Arc<Api>) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        match api.app().tend_devices().await {
            Ok(pass) => {
                if pass.wanted > 0 {
                    tracing::info!(devices = pass.wanted, "block devices wanted");
                }
                for (workload, e) in pass.failed {
                    tracing::warn!(%workload, error = %e, "a workload's devices were not wanted");
                }
            }
            Err(e) => tracing::warn!(error = %e, "tending block devices failed"),
        }
        sweep(&api).await;
    }
}
