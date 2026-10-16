// SPDX-License-Identifier: AGPL-3.0-or-later

//! Charging rate limit buckets (§10 of docs/10-api.md).

use super::StoreResult;
use korpis_core::rate::{Bucket, Standing};
use std::future::Future;
use std::time::SystemTime;
use uuid::Uuid;

/// The buckets every replica charges.
pub trait RateBuckets {
    /// Charges a request at `now` to every bucket of `buckets`, all or none, and says where each
    /// stands: whether it admitted the request, and how long until it is full again.
    fn charge(
        &self,
        buckets: Vec<Bucket>,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Standing>>> + Send;

    /// The organizations the subject keyed `subject_key` holds a grant in that is live at `now`,
    /// its whole chain unrevoked, unsuspended, and unexpired: the organization each one's scope
    /// is anchored in, over every grant it holds.
    fn organizations_held(
        &self,
        subject_key: &str,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;
}
