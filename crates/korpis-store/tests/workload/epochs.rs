// SPDX-License-Identifier: AGPL-3.0-or-later

//! The epoch sequence a restore fences (§8 of docs/03-state.md): issued at most at its rate cap,
//! nothing at all while a restore settles, and never at or below a floor a restore or a node's
//! signed epoch raised.

use crate::support::fixture;
use korpis_app::ports::{EpochIssue, LeaseWrite as _, Store as _, Tx as _};
use std::time::{Duration, SystemTime};

async fn issue(f: &crate::support::Fixture, at: SystemTime) -> EpochIssue {
    let mut tx = f.store.begin().await.unwrap();
    let issued = tx.next_epoch(at).await.unwrap();
    tx.commit().await.unwrap();
    issued
}

#[tokio::test]
async fn epochs_are_issued_at_most_at_the_rate_cap_and_the_bucket_refills() {
    let f = fixture().await;
    sqlx::query("UPDATE epoch_state SET issue_rate_per_sec = 2, bucket_tokens = 2, bucket_at = $1")
        .bind(chrono::DateTime::<chrono::Utc>::from(
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_000),
        ))
        .execute(f.store.pool())
        .await
        .unwrap();
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
    assert_eq!(issue(&f, t).await, EpochIssue::Issued(1));
    assert_eq!(issue(&f, t).await, EpochIssue::Issued(2));
    assert_eq!(issue(&f, t).await, EpochIssue::RateSpent, "a second's worth, spent");
    assert_eq!(issue(&f, t + Duration::from_millis(500)).await, EpochIssue::Issued(3));
    assert_eq!(issue(&f, t + Duration::from_millis(500)).await, EpochIssue::RateSpent);
    let later = t + Duration::from_secs(60);
    assert_eq!(issue(&f, later).await, EpochIssue::Issued(4));
    assert_eq!(issue(&f, later).await, EpochIssue::Issued(5));
    assert_eq!(issue(&f, later).await, EpochIssue::RateSpent, "it holds no more than a second's");
}

#[tokio::test]
async fn nothing_is_issued_while_a_restore_settles_and_nothing_at_or_below_its_floor_after() {
    let f = fixture().await;
    let now = SystemTime::now();
    let mut tx = f.store.begin().await.unwrap();
    tx.fence_epochs(100, now + Duration::from_secs(60)).await.unwrap();
    assert!(tx.settling(now).await.unwrap().is_some());
    tx.commit().await.unwrap();
    assert_eq!(issue(&f, now + Duration::from_secs(1)).await, EpochIssue::Settling);

    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.raise_epoch_floor(150).await.unwrap(), "a node's signed epoch, above the floor");
    assert!(!tx.raise_epoch_floor(120).await.unwrap(), "one below it changes nothing");
    tx.commit().await.unwrap();
    let after = now + Duration::from_secs(61);
    assert_eq!(issue(&f, after).await, EpochIssue::Issued(151));
    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(tx.settling(after).await.unwrap(), None, "the window is over");
    tx.commit().await.unwrap();
}
