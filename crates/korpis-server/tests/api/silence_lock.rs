// SPDX-License-Identifier: AGPL-3.0-or-later

//! The silence sweep takes the placement lock: a placement in progress, which chose its node while
//! the node was heard, binds to it before the sweep marks it unreachable, never after, so no
//! placement lands on a node already found silent and escapes the failover that follows the mark.

use crate::support::Harness;
use korpis_app::ports::{PlacementWrite as _, Store as _, Tx as _};
use std::sync::Arc;

/// Whether some transaction waits for an advisory lock another holds.
async fn waiting(h: &Harness) -> bool {
    let store = h.db.store().await.unwrap();
    let q = "SELECT count(*) > 0 FROM pg_locks WHERE locktype = 'advisory' AND NOT granted";
    sqlx::query_scalar(q).fetch_one(store.pool()).await.unwrap()
}

#[tokio::test]
async fn the_silence_sweep_waits_for_a_placement_in_progress() {
    let h = Harness::new().await;
    h.node(None, "fra-1", &[]).await;
    let store = h.db.store().await.unwrap();
    let silent = "UPDATE node SET connectivity = 'connected', last_seen = now() - interval '1 hour'";
    sqlx::query(silent).execute(store.pool()).await.unwrap();
    let mut placing = store.begin().await.unwrap();
    placing.lock_placement().await.unwrap();

    let api = Arc::clone(&h.api);
    let sweep = tokio::spawn(async move { api.app().sweep_silent_nodes().await });
    // The sweep is blocked on the lock: what marks that is the database's own lock table.
    for _ in 0..400 {
        if waiting(&h).await {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(waiting(&h).await, "the sweep waits for the placement under way");
    let unreachable = "SELECT count(*) FROM node WHERE connectivity = 'unreachable'";
    let marked: i64 = sqlx::query_scalar(unreachable).fetch_one(store.pool()).await.unwrap();
    assert_eq!(marked, 0, "nothing is marked while the placement holds the lock");
    placing.commit().await.unwrap();
    assert_eq!(sweep.await.unwrap().unwrap(), 1, "marked once the placement committed");
}
