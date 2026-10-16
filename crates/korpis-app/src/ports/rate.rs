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

    /// Where each of `buckets` stands at `now`, charging none: whether each would admit a
    /// request. What a check that must not run once a bucket refuses asks first.
    fn peek(
        &self,
        buckets: Vec<Bucket>,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Standing>>> + Send;

    /// Gives back one request charged to `bucket` at `now`: what a charge taken before a check
    /// returns when the check proves it needn't have been taken.
    fn refund(
        &self,
        bucket: &Bucket,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<()>> + Send;

    /// Forgets at most `limit` buckets that are full again at `now`, which a bucket kept nowhere
    /// is too, so a bucket costs a row only while it owes; answers how many it forgot.
    fn forget_full_buckets(
        &self,
        now: SystemTime,
        limit: u32,
    ) -> impl Future<Output = StoreResult<u64>> + Send;

    /// The organizations the subject keyed `subject_key` holds a grant in that is live at `now`,
    /// its whole chain unrevoked, unsuspended, and unexpired: the organization each one's scope
    /// is anchored in, over every grant it holds.
    fn organizations_held(
        &self,
        subject_key: &str,
        now: SystemTime,
    ) -> impl Future<Output = StoreResult<Vec<Uuid>>> + Send;
}
