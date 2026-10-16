// SPDX-License-Identifier: AGPL-3.0-or-later

//! Charging rate limit buckets (§10 of docs/10-api.md).

use super::StoreResult;
use korpis_core::rate::{Bucket, Standing};
use std::future::Future;
use std::time::SystemTime;

/// The buckets every replica charges.
pub trait RateBuckets {
    /// Charges a request at `now` to every bucket of `buckets`, all or none, and says where each
    /// stands: whether it admitted the request, and how long until it is full again.
    fn charge(
        &self,
        buckets: Vec<Bucket>,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Standing>>> + Send;
}
