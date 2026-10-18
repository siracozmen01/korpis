// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a restore recovers a workload to run (Finding 96 of docs/23-walkthroughs.md): to the
//! node that presented it, and between two that did, to the higher lease epoch, never to one
//! that presents an epoch no higher than the store's lease; and under the id slot its node runs
//! it under, unless a workload placed there holds it.

use crate::support::{create, fixture, now};
use korpis_app::ports::{
    PlacementRead, PlacementRecovery, PlacementWrite, RecoveryWrite, SlotRecovery, Store, Tx,
    WorkloadWrite,
};
use korpis_core::placement::{BoundBy, Placement};
use korpis_core::schedule::{Explanation, PlacementPolicy};
use korpis_core::tenancy::quota::Bounds;
use korpis_store::PgStore;
use korpis_store::testing::{enroll, ready_node};
use sqlx::Connection as _;
use uuid::Uuid;

fn on(workload: Uuid, node: Uuid, intent: Uuid, bound_by: BoundBy) -> Placement {
    Placement {
        workload,
        node,
        intent,
        reserved: Bounds::default(),
        bound_at: now(),
        bound_by,
        reason: "recovered".into(),
        explanation: Explanation {
            candidates: 1,
            filtered: Vec::new(),
            scored: Vec::new(),
            chosen: Some(node),
            runner_up: None,
            policy: PlacementPolicy::default(),
        },
        authoritative: true,
        held_at: None,
    }
}

async fn recover(store: &PgStore, p: Placement, epoch: u64) -> PlacementRecovery {
    let mut tx = store.begin().await.unwrap();
    let done = tx.recover_placement(&p, epoch).await.unwrap();
    tx.commit().await.unwrap();
    done
}

async fn nodes(f: &crate::support::Fixture) -> (Uuid, Uuid) {
    let fra3 = ready_node(f.organization, "fra-3", &[]).unwrap();
    let fra4 = ready_node(f.organization, "fra-4", &[]).unwrap();
    enroll(&f.store, &fra3).await.unwrap();
    enroll(&f.store, &fra4).await.unwrap();
    (fra3.id, fra4.id)
}

#[tokio::test]
async fn the_node_that_presented_the_higher_epoch_holds_the_placement() {
    let f = fixture().await;
    let (w, first) = create(&f, "mc").await;
    let (fra3, fra4) = nodes(&f).await;
    let at = |node| on(w.id, node, first.id, BoundBy::Recovery);
    assert_eq!(recover(&f.store, at(fra3), 5).await, PlacementRecovery::Bound { replaced: None });
    let lower = recover(&f.store, at(fra4), 3).await;
    assert_eq!(lower, PlacementRecovery::Elsewhere { node: fra3 }, "a lower epoch");
    let higher = recover(&f.store, at(fra4), 7).await;
    assert_eq!(higher, PlacementRecovery::Bound { replaced: Some(fra3) });
    let equal = recover(&f.store, at(fra3), 7).await;
    assert_eq!(equal, PlacementRecovery::Elsewhere { node: fra4 }, "not higher");
    assert_eq!(recover(&f.store, at(fra4), 1).await, PlacementRecovery::Held);
    let held = f.store.placement(w.id).await.unwrap().unwrap();
    assert_eq!((held.node, held.bound_by), (fra4, BoundBy::Recovery));
}

#[tokio::test]
async fn a_replayed_assignment_does_not_move_what_the_scheduler_placed_under_a_newer_lease() {
    let f = fixture().await;
    let (w, first) = create(&f, "mc").await;
    let (fra3, fra4) = nodes(&f).await;
    let mut tx = f.store.begin().await.unwrap();
    tx.move_target(w.id, first.id, 0).await.unwrap();
    tx.lock_placement().await.unwrap();
    tx.bind(&on(w.id, fra3, first.id, BoundBy::Scheduler)).await.unwrap();
    tx.commit().await.unwrap();
    let mut owner = sqlx::PgConnection::connect(&f.owner_url).await.unwrap();
    sqlx::query(
        "INSERT INTO lease (workload_id, node_id, epoch, term_ms, received_at, grantor_expiry)
         VALUES ($1, $2, 9, 30000, now(), now() + interval '30 seconds')",
    )
    .bind(w.id)
    .bind(fra3)
    .execute(&mut owner)
    .await
    .unwrap();
    let at = |node| on(w.id, node, first.id, BoundBy::Recovery);
    let replayed = recover(&f.store, at(fra4), 4).await;
    assert_eq!(replayed, PlacementRecovery::Elsewhere { node: fra3 }, "below the store's lease");
    let newer = recover(&f.store, at(fra4), 10).await;
    assert_eq!(newer, PlacementRecovery::Bound { replaced: Some(fra3) });
}

#[tokio::test]
async fn a_presented_slot_is_taken_only_from_a_workload_not_placed_on_the_node() {
    let f = fixture().await;
    let (w, _) = create(&f, "mc").await;
    let (gone, _) = create(&f, "gone").await;
    let (here, here_first) = create(&f, "here").await;
    let (node, _) = nodes(&f).await;
    let mut tx = f.store.begin().await.unwrap();
    let given = SlotRecovery::Given { taken: None };
    assert_eq!(tx.recover_id_slot(node, gone.id, 3).await.unwrap(), given);
    let taken = tx.recover_id_slot(node, w.id, 3).await.unwrap();
    assert_eq!(taken, SlotRecovery::Given { taken: Some(gone.id) }, "not placed there");
    assert_eq!(tx.recover_id_slot(node, w.id, 3).await.unwrap(), SlotRecovery::Held);
    tx.move_target(here.id, here_first.id, 0).await.unwrap();
    tx.lock_placement().await.unwrap();
    tx.bind(&on(here.id, node, here_first.id, BoundBy::Scheduler)).await.unwrap();
    assert_eq!(tx.recover_id_slot(node, here.id, 4).await.unwrap(), given);
    let conflict = tx.recover_id_slot(node, w.id, 4).await.unwrap();
    assert_eq!(conflict, SlotRecovery::Conflict { holder: here.id }, "placed there");
    tx.commit().await.unwrap();
    let mut held: Vec<(Uuid, i32)> =
        sqlx::query_as("SELECT workload_id, slot FROM node_id_slot WHERE node_id = $1")
            .bind(node)
            .fetch_all(f.store.pool())
            .await
            .unwrap();
    held.sort_by_key(|(_, slot)| *slot);
    assert_eq!(held, [(w.id, 3), (here.id, 4)], "nothing changed by the conflict");
}
