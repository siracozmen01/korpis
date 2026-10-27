// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes (§7 of docs/06-storage.md): kept from what nodes report, detached and kept when their
//! workload retires, marked when deleted and never removed, and found under their project.

use crate::support::{create, fixture, now};
use korpis_app::Page;
use korpis_app::ports::{HierarchyRead, RetirementWrite, Store, Tx, VolumeRead, VolumeWrite};
use korpis_core::authz::{Kind, ObjectRef};
use korpis_core::workload::volume::{ReportedVolume, VolumeEvent, VolumeKind};
use korpis_store::testing::{enroll, ready_node};
use uuid::Uuid;

fn reported(workload: Uuid, name: &str, helper_id: &str, event: VolumeEvent) -> ReportedVolume {
    ReportedVolume {
        workload,
        name: name.into(),
        helper_id: helper_id.into(),
        storage_class: "local".into(),
        size_bytes: 1 << 30,
        quota_enforced: true,
        event,
        kind: VolumeKind::Declared,
    }
}

#[tokio::test]
async fn a_volume_outlives_its_workload_and_is_marked_when_deleted() {
    let f = fixture().await;
    let (first, _) = create(&f, "a").await;
    let (second, _) = create(&f, "b").await;
    let node = ready_node(f.organization, "fra-1", &[]).unwrap();
    enroll(&f.store, &node).await.unwrap();

    let mut tx = f.store.begin().await.unwrap();
    let ensured = reported(first.id, "data", "a.data", VolumeEvent::Ensured);
    tx.record_volume(node.id, &ensured, None, now()).await.unwrap();
    tx.record_volume(node.id, &ensured, None, now()).await.unwrap();
    tx.commit().await.unwrap();
    let listed = f.store.volumes(f.project, None, false, Page::default()).await.unwrap();
    assert_eq!(listed.len(), 1, "ensured twice, one volume");
    let v = listed[0].clone();
    assert_eq!((v.created_by, v.attached_to, v.node), (first.id, Some(first.id), Some(node.id)));
    let chain = f.store.ancestry(ObjectRef::new(Kind::Volume, v.id)).await.unwrap();
    let kinds: Vec<_> = chain.iter().map(|link| link.object.kind).collect();
    assert_eq!(kinds, [Kind::Volume, Kind::Project, Kind::Organization]);

    let mut tx = f.store.begin().await.unwrap();
    assert!(tx.retire_workload(first.id, now()).await.unwrap());
    tx.commit().await.unwrap();
    let kept = f.store.volume(v.id).await.unwrap().unwrap();
    assert_eq!(kept.attached_to, None, "detached when its workload retired");
    assert!(kept.detached_at.is_some() && kept.deleted_at.is_none(), "and kept");

    let mut tx = f.store.begin().await.unwrap();
    let adopted = ReportedVolume { workload: second.id, ..ensured.clone() };
    tx.record_volume(node.id, &adopted, None, now()).await.unwrap();
    tx.commit().await.unwrap();
    let held = f.store.volume(v.id).await.unwrap().unwrap();
    assert_eq!(
        (held.created_by, held.attached_to),
        (first.id, Some(second.id)),
        "adopted, same volume"
    );
    assert_eq!(
        f.store.volumes(f.project, Some(second.id), false, Page::default()).await.unwrap().len(),
        1
    );

    let mut tx = f.store.begin().await.unwrap();
    let gone = reported(second.id, "data", "a.data", VolumeEvent::Deleted);
    tx.record_volume(node.id, &gone, None, now()).await.unwrap();
    tx.record_volume(node.id, &ensured, None, now()).await.unwrap();
    tx.commit().await.unwrap();
    let live = f.store.volumes(f.project, None, false, Page::default()).await.unwrap();
    assert_eq!(live.len(), 1, "the name ensured again after deletion is a new volume");
    assert_ne!(live[0].id, v.id);
    let all = f.store.volumes(f.project, None, true, Page::default()).await.unwrap();
    assert_eq!(all.len(), 2, "the deleted one is kept");
    let deleted = f.store.volume(v.id).await.unwrap().unwrap();
    assert!(deleted.deleted_at.is_some() && deleted.attached_to.is_none());
}

#[tokio::test]
async fn a_shared_volume_is_its_store_s_whichever_node_reports_it() {
    let f = fixture().await;
    let (db, _) = create(&f, "db").await;
    let (a, b) = (
        ready_node(f.organization, "a", &[]).unwrap(),
        ready_node(f.organization, "b", &[]).unwrap(),
    );
    enroll(&f.store, &a).await.unwrap();
    enroll(&f.store, &b).await.unwrap();
    let store = Some("0b9c1f40-3d2e-4c55-9f0a-7e6d5c4b3a21");
    let local = reported(db.id, "data", "db.data", VolumeEvent::Ensured);
    let ensured = ReportedVolume { storage_class: "shared".into(), ..local.clone() };

    let mut tx = f.store.begin().await.unwrap();
    tx.record_volume(a.id, &ensured, store, now()).await.unwrap();
    tx.record_volume(b.id, &ensured, store, now()).await.unwrap();
    tx.record_volume(a.id, &local, None, now()).await.unwrap();
    tx.commit().await.unwrap();
    let listed = f.store.volumes(f.project, None, false, Page::default()).await.unwrap();
    assert_eq!(listed.len(), 2, "one in the store, reported twice, and one on a's disk");
    let shared = listed.iter().find(|v| v.store.is_some()).unwrap();
    assert_eq!((shared.node, shared.store.as_deref(), shared.reported_by), (None, store, b.id));
    assert_eq!(shared.keeper(), b.id, "kept by the node that last reported it");

    let (stranger, _) = create(&f, "stranger").await;
    let mut tx = f.store.begin().await.unwrap();
    let taken = ReportedVolume { workload: stranger.id, ..ensured.clone() };
    assert!(!tx.record_volume(a.id, &taken, store, now()).await.unwrap(), "not the stranger's");
    let deleted = ReportedVolume { event: VolumeEvent::Deleted, ..taken };
    assert!(!tx.record_volume(a.id, &deleted, store, now()).await.unwrap(), "nor deleted by it");
    tx.commit().await.unwrap();
    let kept = f.store.volume(shared.id).await.unwrap().unwrap();
    assert_eq!((kept.attached_to, kept.deleted_at), (Some(db.id), None), "left as it was");

    let mut tx = f.store.begin().await.unwrap();
    let gone = ReportedVolume { event: VolumeEvent::Deleted, ..ensured };
    tx.record_volume(a.id, &gone, store, now()).await.unwrap();
    tx.commit().await.unwrap();
    let live = f.store.volumes(f.project, None, false, Page::default()).await.unwrap();
    assert_eq!(live.len(), 1, "deleted by a node other than its keeper, and a's own kept");
    assert_eq!(live[0].node, Some(a.id));
}
