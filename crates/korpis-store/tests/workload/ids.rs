// SPDX-License-Identifier: AGPL-3.0-or-later

//! User-namespace id slots (§4.5 of docs/04-runtimes.md): unique per node while a workload is
//! there, kept by the workload, and given back when it is retired.

use crate::support::{create, fixture, now};
use korpis_app::ports::{AssignmentWrite, RetirementWrite, Store, Tx};
use korpis_store::testing::{enroll, ready_node};

#[tokio::test]
async fn slots_are_unique_per_node_kept_and_given_back_at_retirement() {
    let f = fixture().await;
    let (a, _) = create(&f, "a").await;
    let (b, _) = create(&f, "b").await;
    let (c, _) = create(&f, "c").await;
    let one = ready_node(f.organization, "fra-1", &[]).unwrap();
    let two = ready_node(f.organization, "fra-2", &[]).unwrap();
    enroll(&f.store, &one).await.unwrap();
    enroll(&f.store, &two).await.unwrap();

    let mut tx = f.store.begin().await.unwrap();
    assert_eq!(tx.id_slot(one.id, a.id, 4).await.unwrap(), Some(0));
    assert_eq!(tx.id_slot(one.id, b.id, 4).await.unwrap(), Some(1));
    assert_eq!(tx.id_slot(one.id, a.id, 4).await.unwrap(), Some(0), "kept");
    assert_eq!(tx.id_slot(two.id, b.id, 4).await.unwrap(), Some(0), "unique per node only");
    assert_eq!(tx.id_slot(one.id, c.id, 2).await.unwrap(), None, "every slot is held");
    assert!(tx.retire_workload(a.id, now()).await.unwrap());
    assert_eq!(tx.id_slot(one.id, c.id, 2).await.unwrap(), Some(0), "a's slot came back");
    tx.commit().await.unwrap();
}
