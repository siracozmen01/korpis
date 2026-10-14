// SPDX-License-Identifier: AGPL-3.0-or-later

//! Snapshots (§4 of docs/06-storage.md): taken by a plan that applies at once, in a class that
//! declares it can, under `volume.snapshot.create`; rolled back to by a plan that stops and starts
//! the running workload around the rollback and waits for a person; deleted the same way; and
//! kept as the node reports them.

use crate::agent::{Stream, TestAgent, welcome};
use crate::journal::{ack, assignment, complete, send, until};
use crate::lifecycle::approve;
use crate::recipes::{body, create, project, push_paper};
use crate::streams::held_and_released;
use crate::support::{Harness, mutation, reason};
use crate::volumes::{ensured, holding, list, volume_record};
use connectrpc::ErrorCode;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

/// A storage class that can snapshot, named as the volumes the test reports name it.
fn snapshotting() -> pb::StorageClassInfo {
    pb::StorageClassInfo {
        name: "local".into(),
        backend: pb::StorageBackend::STORAGE_BACKEND_BTRFS.into(),
        capabilities: pb::StorageCapabilities {
            snapshot: true,
            snapshot_atomic: true,
            quota_enforced: true,
            quota_capacity: true,
            fencing: pb::Fencing::FENCING_NONE.into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }
}

fn reported(
    seq: u64,
    workload: &str,
    snapshot: &str,
    event: agent::SnapshotEventKind,
) -> agent::JournalRecord {
    let e = agent::SnapshotEvent {
        workload: workload.to_owned(),
        volume: format!("{workload}.data"),
        snapshot: snapshot.to_owned(),
        name: "before-update".into(),
        event: event.into(),
        ..Default::default()
    };
    agent::JournalRecord {
        seq,
        body: Some(B::Snapshot(Box::new(e))),
        count: 1,
        ..Default::default()
    }
}

/// A node declaring `storage`, running a workload with a reported volume `data`: the harness,
/// the node's stream, the workload, the volume, and the next journal sequence.
async fn running(storage: Vec<pb::StorageClassInfo>) -> (Harness, Stream, String, String, u64) {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (mut s, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], storage)).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(create("w", &project, "mc", body(&repo, &[])));
    let declared = declared.await.unwrap().into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    let created = until(&mut s, assignment(|a| a.plan.is_set())).await;
    let last = complete(&mut s, &workload, created.plan.as_option().unwrap(), 1).await;
    held_and_released(&mut s, &workload).await;
    send(&mut s, vec![volume_record(last + 1, ensured(&workload, "data", true, false))]).await;
    assert_eq!(until(&mut s, ack).await, last + 1);
    let listed = h.volumes(h.root()).list_volumes(list(&project, false)).await.unwrap();
    let volume = listed.into_owned().volumes[0].id.clone();
    (h, s, workload, volume, last + 2)
}

fn take(volume: &str, name: &str) -> pb::CreateSnapshotRequest {
    pb::CreateSnapshotRequest {
        mutation: mutation(&format!("snap-{name}")).into(),
        volume: volume.to_owned(),
        name: name.to_owned(),
        ..Default::default()
    }
}

/// The node's next plan with a step named `action`, completed; the last sequence used.
async fn executed(
    s: &mut Stream,
    workload: &str,
    action: &str,
    from: u64,
) -> (agent::NodePlan, u64) {
    let has = |a: &agent::Assignment| {
        a.plan.as_option().is_some_and(|p| p.steps.iter().any(|s| s.action == action))
    };
    let told = until(s, assignment(has)).await;
    let plan = told.plan.as_option().unwrap().clone();
    let last = complete(s, workload, &plan, from).await;
    (plan, last)
}

/// Deletes `snapshot` (its wire and bare forms) of `volume` by an approved plan, as the node
/// running `workload` reports it from journal sequence `last + 2`.
async fn deleted(
    h: &Harness,
    s: &mut Stream,
    (workload, volume): (&str, &str),
    (snapshot, bare): (&str, &str),
    last: u64,
) {
    let volumes = h.volumes(h.root());
    let listed = pb::ListSnapshotsRequest { volume: volume.to_owned(), ..Default::default() };
    let gone = pb::DeleteSnapshotRequest {
        mutation: mutation("gone").into(),
        id: snapshot.to_owned(),
        ..Default::default()
    };
    let deleting = volumes.delete_snapshot(gone).await.unwrap().into_owned().plan.into_option();
    let deleting = deleting.unwrap();
    assert_eq!(deleting.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED));
    h.plans(h.root()).approve_plan(approve("ds", &deleting.id)).await.unwrap();
    let (_, last) = executed(s, workload, "delete_snapshot", last + 2).await;
    let deleted = agent::SnapshotEventKind::SNAPSHOT_EVENT_KIND_DELETED;
    send(s, vec![reported(last + 1, workload, bare, deleted)]).await;
    assert_eq!(until(s, ack).await, last + 1);
    assert!(
        volumes.list_snapshots(listed.clone()).await.unwrap().into_owned().snapshots.is_empty()
    );
    let all = pb::ListSnapshotsRequest { include_deleted: true, ..listed };
    let kept = volumes.list_snapshots(all).await.unwrap().into_owned().snapshots;
    assert!(kept[0].deleted_at.is_set(), "kept, and marked");
}

#[tokio::test]
async fn a_snapshot_is_taken_rolled_back_to_and_deleted() {
    let (h, mut s, workload, volume, next) = running(vec![snapshotting()]).await;
    let volumes = h.volumes(h.root());
    let reader = holding(&h, &["volume.read"]).await;
    let e = h.volumes(reader).create_snapshot(take(&volume, "no")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "volume.snapshot.create is needed");
    let made = volumes.create_snapshot(take(&volume, "before-update")).await.unwrap().into_owned();
    let plan = made.plan.as_option().unwrap();
    assert_eq!(plan.kind.as_known(), Some(pb::PlanKind::PLAN_KIND_SNAPSHOT_VOLUME));
    assert_ne!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED), "at once");
    assert!(made.snapshot.starts_with("snp_"));
    let e = volumes.create_snapshot(take(&volume, "second")).await.unwrap_err();
    assert_eq!(reason(&e), "volume.busy", "one plan on a volume at a time");
    let (_, last) = executed(&mut s, &workload, "snapshot_volume", next).await;
    let bare = korpis_core::id::Id::parse(korpis_core::id::IdKind::Snapshot, &made.snapshot);
    let bare = bare.unwrap().uuid().to_string();
    let taken = agent::SnapshotEventKind::SNAPSHOT_EVENT_KIND_TAKEN;
    send(&mut s, vec![reported(last + 1, &workload, &bare, taken)]).await;
    assert_eq!(until(&mut s, ack).await, last + 1);
    let listed = pb::ListSnapshotsRequest { volume: volume.clone(), ..Default::default() };
    let snapshots = volumes.list_snapshots(listed.clone()).await.unwrap().into_owned().snapshots;
    assert_eq!(snapshots.len(), 1);
    assert_eq!(
        (snapshots[0].id.as_str(), snapshots[0].name.as_str()),
        (made.snapshot.as_str(), "before-update")
    );
    let got = pb::GetVolumeRequest { id: volume.clone(), ..Default::default() };
    let v = volumes.get_volume(got).await.unwrap().into_owned().volume.into_option().unwrap();
    assert_eq!(v.snapshots, std::slice::from_ref(&made.snapshot), "listed on the volume");

    let back = pb::RollbackVolumeRequest {
        mutation: mutation("back").into(),
        snapshot: made.snapshot.clone(),
        ..Default::default()
    };
    let rollback = volumes.rollback_volume(back).await.unwrap().into_owned().plan.into_option();
    let rollback = rollback.unwrap();
    assert_eq!(rollback.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED), "a person");
    let actions: Vec<&str> = rollback.steps.iter().map(|s| s.action.as_str()).collect();
    assert_eq!(actions, ["stop", "rollback_volume", "start"], "around the running workload");
    h.plans(h.root()).approve_plan(approve("rb", &rollback.id)).await.unwrap();
    let (_, last) = executed(&mut s, &workload, "rollback_volume", last + 2).await;
    let rolled = agent::SnapshotEventKind::SNAPSHOT_EVENT_KIND_ROLLED_BACK;
    send(&mut s, vec![reported(last + 1, &workload, &bare, rolled)]).await;
    assert_eq!(until(&mut s, ack).await, last + 1);
    assert_eq!(h.effects("volume.snapshot.rolled_back").await.len(), 1);

    deleted(&h, &mut s, (&workload, &volume), (&made.snapshot, &bare), last).await;
}

#[tokio::test]
async fn a_class_that_cannot_snapshot_is_refused_with_its_reason() {
    let (h, _s, _, volume, _) = running(Vec::new()).await;
    let e = h.volumes(h.root()).create_snapshot(take(&volume, "x")).await.unwrap_err();
    assert_eq!(reason(&e), "volume.cannot_snapshot");
}
