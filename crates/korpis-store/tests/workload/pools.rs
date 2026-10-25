// SPDX-License-Identifier: AGPL-3.0-or-later

//! Connections under contention (Finding 177 of docs/23-walkthroughs.md): more transactions
//! waiting for the placement lock than the store has connections, the one holding it reading
//! outside its transaction, as placement does. In one pool the waiters held every connection and
//! the holder waited for one until the pool gave up; with transactions and reads apart, every
//! one finishes at once.

use korpis_app::ports::{NodeRead, PlacementWrite, Store, Tx};
use korpis_store::testing::TestDb;
use std::time::Duration;
use uuid::Uuid;

#[tokio::test]
async fn transactions_waiting_for_a_lock_never_starve_the_one_holding_it_of_a_read() {
    let db = TestDb::new().await.unwrap();
    let store = db.store().await.unwrap();
    let (reads, transactions) = korpis_store::split(korpis_store::testing::CONNECTIONS);
    let waiting = usize::try_from(reads + transactions).unwrap() + 2;
    let one = |store: korpis_store::PgStore| async move {
        let mut tx = store.begin().await.unwrap();
        tx.lock_placement().await.unwrap();
        // What placement reads outside its transaction while it holds the lock.
        store.node(Uuid::nil()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        tx.commit().await.unwrap();
    };
    let all: Vec<_> = (0..waiting).map(|_| tokio::spawn(one(store.clone()))).collect();
    let done = tokio::time::timeout(Duration::from_secs(10), async {
        for t in all {
            t.await.unwrap();
        }
    });
    assert!(done.await.is_ok(), "every placement finishes, one after another");
}
