// SPDX-License-Identifier: AGPL-3.0-or-later

//! Where a restore recovers a workload to run (Finding 96 of docs/23-walkthroughs.md): to the
//! node that presented it, and between two that did, to the higher lease epoch; and under the
//! id slot its node runs it under.

use crate::support::{create, fixture, now};
use korpis_app::ports::{PlacementRead, PlacementRecovery, RecoveryWrite, Store, Tx};
use korpis_core::placement::{BoundBy, Placement};
use korpis_core::schedule::{Explanation, PlacementPolicy};
use korpis_core::tenancy::quota::Bounds;
use korpis_store::testing::{enroll, ready_node};
use uuid::Uuid;

fn on(workload: Uuid, node: Uuid, intent: Uuid) -> Placement {
    Placement {
        workload,
        node,
        intent,
        reserved: Bounds::default(),
        bound_at: now(),
        bound_by: BoundBy::Recovery,
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
    }
}

#[tokio::test]
async fn the_node_that_presented_the_higher_epoch_holds_the_placement() {
    let f = fixture().await;
    let (w, first) = create(&f, "mc").await;
    let fra3 = ready_node(f.organization, "fra-3", &[]).unwrap();
    let fra4 = ready_node(f.organization, "fra-4", &[]).unwrap();
    enroll(&f.store, &fra3).await.unwrap();
    enroll(&f.store, &fra4).await.unwrap();
    let recover = |node: Uuid, epoch: u64| {
        let p = on(w.id, node, first.id);
        let store = f.store.clone();
        async move {
            let mut tx = store.begin().await.unwrap();
            let done = tx.recover_placement(&p, epoch).await.unwrap();
            tx.commit().await.unwrap();
            done
        }
    };
    assert_eq!(recover(fra3.id, 5).await, PlacementRecovery::Bound { replaced: None });
    assert_eq!(recover(fra4.id, 3).await, PlacementRecovery::Kept, "a lower epoch");
    assert_eq!(recover(fra4.id, 7).await, PlacementRecovery::Bound { replaced: Some(fra3.id) });
    assert_eq!(recover(fra3.id, 7).await, PlacementRecovery::Kept, "not higher");
    assert_eq!(recover(fra4.id, 1).await, PlacementRecovery::Kept, "where it is already");
    let held = f.store.placement(w.id).await.unwrap().unwrap();
    assert_eq!((held.node, held.bound_by), (fra4.id, BoundBy::Recovery));
}

#[tokio::test]
async fn a_presented_slot_is_taken_from_whichever_workload_the_store_gave_it() {
    let f = fixture().await;
    let (w, _) = create(&f, "mc").await;
    let (other, _) = create(&f, "other").await;
    let node = ready_node(f.organization, "fra-3", &[]).unwrap();
    enroll(&f.store, &node).await.unwrap();
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.recover_id_slot(node.id, other.id, 3).await.unwrap(), "given");
    assert!(tx.recover_id_slot(node.id, w.id, 3).await.unwrap(), "taken from other");
    assert!(!tx.recover_id_slot(node.id, w.id, 3).await.unwrap(), "held as presented");
    tx.commit().await.unwrap();
    let held: Vec<(Uuid, i32)> =
        sqlx::query_as("SELECT workload_id, slot FROM node_id_slot WHERE node_id = $1")
            .bind(node.id)
            .fetch_all(f.store.pool())
            .await
            .unwrap();
    assert_eq!(held, [(w.id, 3)]);
}
