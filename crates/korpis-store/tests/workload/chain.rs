// SPDX-License-Identifier: AGPL-3.0-or-later

//! The intent chain, the head and target pointers, and power (§3.2 of docs/03-state.md).

use crate::support::{body, create, fixture, intent, power};
use korpis_app::Page;
use korpis_app::ports::{
    Store, StoreError, Tx, Unique, WorkloadFilter, WorkloadRead, WorkloadWrite,
};
use korpis_core::workload::{Power, PowerChange, PowerState};
use sqlx::Row;

#[tokio::test]
async fn a_workload_is_created_with_its_first_intent_and_power() {
    let f = fixture().await;
    let (w, first) = create(&f, "mc").await;
    let read = f.store.workload(w.id).await.unwrap().unwrap();
    assert_eq!(read.name, w.name);
    assert_eq!(read.labels, w.labels);
    assert_eq!((read.intent, read.intent_version, read.target), (first.id, 1, None));
    assert_eq!(f.store.intent(first.id).await.unwrap().unwrap(), first);
    let p = f.store.power(w.id).await.unwrap().unwrap();
    assert_eq!(p.power, Power::initial(PowerState::Running));
    let filter = WorkloadFilter { project: Some(f.project), ..Default::default() };
    assert_eq!(f.store.workloads(&filter, Page::default()).await.unwrap().len(), 1);

    let mut tx = f.store.begin().await.unwrap();
    let again = intent(uuid::Uuid::now_v7(), 1, None, "x");
    let mut twin = w.clone();
    twin.id = again.workload;
    let e = tx
        .create_workload(&twin, &again, &power(twin.id, Power::initial(PowerState::Running)))
        .await
        .unwrap_err();
    assert_eq!(e, StoreError::Duplicate(Unique::WorkloadName));
}

#[tokio::test]
async fn the_body_is_stored_in_its_wire_form_with_defaults_written() {
    let f = fixture().await;
    let (_, first) = create(&f, "mc").await;
    let row = sqlx::query("SELECT body, schema_version FROM intent WHERE id = $1")
        .bind(first.id)
        .fetch_one(f.store.pool())
        .await
        .unwrap();
    let json: serde_json::Value = row.get("body");
    assert_eq!(row.get::<String, _>("schema_version"), "korpis.v1");
    assert_eq!(json["schedule"], "", "a default is written, not omitted (§3.1, R10)");
    assert_eq!(json["recipe"]["digest"], first.body.recipe.digest);
    assert_eq!(json["config"]["motd"]["stringValue"], "hello");
}

#[tokio::test]
async fn declaring_moves_the_head_once_and_never_the_target() {
    let f = fixture().await;
    let (w, first) = create(&f, "mc").await;
    let second = intent(w.id, 2, Some(first.id), "two");
    let rival = intent(w.id, 2, Some(first.id), "rival");
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.append_intent(&second).await.unwrap());
    assert!(!tx.append_intent(&rival).await.unwrap(), "the head moved: a conflict");
    tx.commit().await.unwrap();
    let read = f.store.workload(w.id).await.unwrap().unwrap();
    assert_eq!((read.intent, read.intent_version, read.target), (second.id, 2, None));
    let chain = f.store.intents(w.id, Page::default()).await.unwrap();
    assert_eq!(chain.iter().map(|i| i.version).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(chain[1].body, body("two"));
}

#[tokio::test]
async fn only_the_head_is_approvable_and_the_target_moves_by_compare_and_swap() {
    let f = fixture().await;
    let (w, first) = create(&f, "mc").await;
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.move_target(w.id, first.id, 0).await.unwrap());
    assert!(!tx.move_target(w.id, first.id, 0).await.unwrap(), "a stale target version");
    let second = intent(w.id, 2, Some(first.id), "two");
    assert!(tx.append_intent(&second).await.unwrap());
    assert!(!tx.move_target(w.id, first.id, 1).await.unwrap(), "no longer the head");
    assert!(tx.move_target(w.id, second.id, 1).await.unwrap());
    tx.commit().await.unwrap();
    let read = f.store.workload(w.id).await.unwrap().unwrap();
    assert_eq!((read.target, read.target_version), (Some(second.id), 2));
}

#[tokio::test]
async fn power_is_its_own_sequence() {
    let f = fixture().await;
    let (w, first) = create(&f, "mc").await;
    let start = Power::initial(PowerState::Running);
    let stop = power(w.id, start.next(PowerChange::Stop));
    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.append_power(&stop).await.unwrap());
    assert!(!tx.append_power(&stop).await.unwrap(), "version 2 is taken");
    tx.commit().await.unwrap();
    let read = f.store.workload(w.id).await.unwrap().unwrap();
    assert_eq!((read.power_version, read.intent), (2, first.id), "the intent chain is untouched");
    assert_eq!(f.store.power(w.id).await.unwrap().unwrap().power.desired, PowerState::Stopped);
}
