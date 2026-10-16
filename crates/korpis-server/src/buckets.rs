// SPDX-License-Identifier: AGPL-3.0-or-later

//! Forgetting the rate limit buckets that are full again (§10 of docs/10-api.md): a bucket kept
//! nowhere is full, so a bucket costs a row only while it owes, and the addresses and invented
//! sign-in names a flood brings leave nothing behind once their buckets refill.

use crate::api::Api;
use korpis_app::ports::RateBuckets as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// How often the buckets are swept.
const EVERY: Duration = Duration::from_secs(60);
/// How many buckets one statement forgets at most, so none holds its rows long.
const BATCH: u32 = 1_000;

/// Forgets, every minute, the buckets full again, a batch at a time until none is left. The task
/// ends with the process.
pub async fn forget(api: Arc<Api>) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        loop {
            match api.app().store().forget_full_buckets(SystemTime::now(), BATCH).await {
                Ok(n) if n < u64::from(BATCH) => break,
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "forgetting full rate limit buckets failed");
                    break;
                }
            }
        }
    }
}
