// SPDX-License-Identifier: AGPL-3.0-or-later

//! The event log (§5 of docs/15-observability.md, migration 0023): every effect is an event, a
//! key's events are numbered in commit order, and positions are given only below the snapshot
//! horizon, so delivering by position never passes over an event that commits late.

use crate::support::fixture;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Appends an effect authorized by `grant`, which keys its event to the grant's organization.
async fn effect(tx: &mut Transaction<'_, Postgres>, grant: Uuid, action: &str) {
    sqlx::query(
        "INSERT INTO effect (id, actor, action, outcome, grant_id)
         VALUES ($1, 'system:bootstrap', $2, 'succeeded', $3)",
    )
    .bind(Uuid::now_v7())
    .bind(action)
    .bind(grant)
    .execute(&mut **tx)
    .await
    .unwrap();
}

/// Sequences until `n` events are positioned in all. The horizon is the whole cluster's, so a
/// transaction another test holds open, in another database, can hold positions back a while.
async fn sequenced(store: &korpis_store::PgStore, n: u64) {
    let mut total = 0;
    // Up to a minute: the horizon is the shared test server's, which other tests' teardowns hold
    // back for seconds at a time (§5 of docs/15-observability.md).
    for _ in 0..1200 {
        total += store.sequence_events().await.unwrap();
        if total >= n {
            assert_eq!(total, n);
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("{total} of {n} events positioned");
}

/// Gives `tx` its transaction identifier now, as a first write would.
async fn begin_writing(tx: &mut Transaction<'_, Postgres>) {
    sqlx::query("SELECT pg_current_xact_id()").execute(&mut **tx).await.unwrap();
}

#[tokio::test]
async fn positions_follow_a_keys_commit_order_and_wait_for_every_earlier_transaction() {
    let f = fixture().await;
    let pool = f.store.pool();
    // A starts writing first, so it holds the older transaction; B overtakes it.
    let mut a = pool.begin().await.unwrap();
    begin_writing(&mut a).await;
    let mut b = pool.begin().await.unwrap();
    begin_writing(&mut b).await;
    effect(&mut b, f.grant, "b.first").await;
    b.commit().await.unwrap();
    assert_eq!(f.store.sequence_events().await.unwrap(), 0, "A could still commit an event");

    effect(&mut a, f.grant, "a.second").await;
    a.commit().await.unwrap();
    sequenced(&f.store, 2).await;
    let order: Vec<(String, i64, i64)> =
        sqlx::query_as("SELECT kind, seq, position FROM event ORDER BY position")
            .fetch_all(pool)
            .await
            .unwrap();
    let key: String =
        sqlx::query_scalar("SELECT DISTINCT key FROM event").fetch_one(pool).await.unwrap();
    assert_eq!(key, format!("organization:{}", f.organization));
    assert_eq!(
        order,
        [("b.first".to_owned(), 1, 1), ("a.second".to_owned(), 2, 2)],
        "the key's numbering is its commit order, and positions agree with it"
    );
}

#[tokio::test]
async fn an_effect_about_no_organization_is_no_event_and_old_events_are_pruned() {
    let f = fixture().await;
    let pool = f.store.pool();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query(
        "INSERT INTO effect (id, actor, action, outcome)
         VALUES ($1, 'system:bootstrap', 'installation.bootstrap', 'succeeded')",
    )
    .bind(Uuid::now_v7())
    .execute(&mut *tx)
    .await
    .unwrap();
    effect(&mut tx, f.grant, "org.write").await;
    tx.commit().await.unwrap();
    sequenced(&f.store, 1).await;
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event").fetch_one(pool).await.unwrap();
    assert_eq!(events, 1, "only the effect with an organization is an event");

    sqlx::query("UPDATE event SET at = now() - interval '31 days'").execute(pool).await.unwrap();
    let month = std::time::Duration::from_secs(30 * 24 * 3600);
    assert_eq!(f.store.prune_events(month).await.unwrap(), 1);
    let effects: i64 =
        sqlx::query_scalar("SELECT count(*) FROM effect").fetch_one(pool).await.unwrap();
    assert_eq!(effects, 2, "pruning the log never touches the audit record");
}
