// SPDX-License-Identifier: AGPL-3.0-or-later

//! Rate limit buckets (§10 of docs/10-api.md): a peek charges nothing, a refund gives a request
//! back and never more than the bucket holds, and a bucket full again is forgotten, which changes
//! nothing a charge answers.

use crate::support::{fixture, now};
use korpis_app::ports::RateBuckets as _;
use korpis_core::rate::{Bucket, Rate};
use std::time::Duration;

#[tokio::test]
async fn a_peek_charges_nothing_and_a_full_bucket_is_forgotten_as_if_kept() {
    let f = fixture().await;
    let bucket =
        || Bucket { policy: "guesses", key: "n:friend".into(), rate: Rate::per_second(1, 2) };
    let at = now();
    for _ in 0..2 {
        assert!(f.store.charge(vec![bucket()], at).await.unwrap()[0].admitted);
    }
    for _ in 0..3 {
        assert!(!f.store.peek(vec![bucket()], at).await.unwrap()[0].admitted, "empty");
    }
    assert_eq!(f.store.forget_full_buckets(at, 100).await.unwrap(), 0, "it still owes");
    let later = at + Duration::from_secs(2);
    assert!(f.store.peek(vec![bucket()], later).await.unwrap()[0].admitted, "full again");
    assert_eq!(f.store.forget_full_buckets(later, 100).await.unwrap(), 1);
    let charged = f.store.charge(vec![bucket()], later).await.unwrap();
    assert!(charged[0].admitted, "a forgotten bucket is a full one");
    assert_eq!(charged[0].remaining(), 1);
    f.store.refund(&bucket(), later).await.unwrap();
    f.store.refund(&bucket(), later).await.unwrap();
    let peeked = f.store.peek(vec![bucket()], later).await.unwrap();
    assert_eq!(peeked[0].remaining(), 2, "given back, and never past full");
}
