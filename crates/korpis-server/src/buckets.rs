// SPDX-License-Identifier: AGPL-3.0-or-later

//! Forgetting what has lapsed: the rate limit buckets that are full again (§10 of
//! docs/10-api.md), since a bucket kept nowhere is full, so the addresses and invented sign-in
//! names a flood brings leave nothing behind once their buckets refill; the user sessions ended
//! by age or idleness (§9.4 of docs/08-identity.md), with the credentials they rotated; and the
//! device authorizations that expired undecided or unspent.

use crate::api::Api;
use korpis_app::ports::{DeviceGrantRead as _, RateBuckets as _, UserSessionRead as _};
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

/// Forgets, every minute, the user sessions ended by age or idleness and the device
/// authorizations expired, a batch at a time until none is left. The task ends with the process.
pub async fn forget_sessions(api: Arc<Api>) {
    let mut tick = tokio::time::interval(EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let idle = api.app().session_limits().idle;
    loop {
        tick.tick().await;
        loop {
            match api.app().store().forget_lapsed_sessions(SystemTime::now(), idle, BATCH).await {
                Ok(n) if n < u64::from(BATCH) => break,
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "forgetting lapsed sessions failed");
                    break;
                }
            }
        }
        loop {
            let store = api.app().store();
            match store.forget_expired_device_authorizations(SystemTime::now(), BATCH).await {
                Ok(n) if n < u64::from(BATCH) => break,
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "forgetting expired device authorizations failed");
                    break;
                }
            }
        }
    }
}
