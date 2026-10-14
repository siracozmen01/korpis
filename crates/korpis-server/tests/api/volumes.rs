// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes (§7 of docs/06-storage.md): kept from what the node journals, believed only from a
//! node that once held the workload's lease, read under `volume.read` on their project, and a
//! size shown only where the write path enforces it (Rule K-3).

use crate::agent::{Stream, TestAgent, welcome};
use crate::journal::{ack, assignment, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::streams::{held_and_released, segment_record};
use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use korpis_core::principal::Principal;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::principal;
use std::time::SystemTime;

pub(crate) fn volume_record(seq: u64, e: agent::VolumeEvent) -> agent::JournalRecord {
    agent::JournalRecord { seq, body: Some(B::Volume(Box::new(e))), count: 1, ..Default::default() }
}

pub(crate) fn ensured(
    workload: &str,
    name: &str,
    quota_enforced: bool,
    stream: bool,
) -> agent::VolumeEvent {
    agent::VolumeEvent {
        workload: workload.to_owned(),
        name: name.to_owned(),
        helper_id: format!("{workload}.{name}"),
        storage_class: "local".into(),
        size_bytes: 1 << 30,
        quota_enforced,
        event: agent::VolumeEventKind::VOLUME_EVENT_KIND_ENSURED.into(),
        stream,
        ..Default::default()
    }
}

pub(crate) fn list(project: &str, deleted: bool) -> pb::ListVolumesRequest {
    pb::ListVolumesRequest {
        project: project.to_owned(),
        include_deleted: deleted,
        ..Default::default()
    }
}

/// A node holding a workload it once held the lease of: the harness, the node's stream, the
/// project, and the workload.
pub(crate) async fn placed() -> (Harness, Stream, String, String) {
    let (h, s, project, workload, _) = placing().await;
    (h, s, project, workload)
}

/// As [`placed`], with the assignment that carried the workload's first plan. Boxed, since what
/// it holds is large for a future that every test awaits.
pub(crate) fn placing() -> std::pin::Pin<Box<impl Future<Output = Placing>>> {
    Box::pin(start_placing())
}

/// The harness, the node's stream, the project, the workload, and its first assignment.
pub(crate) type Placing = (Harness, Stream, String, String, agent::Assignment);

async fn start_placing() -> Placing {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    let created = until(&mut s, assignment(|a| a.plan.is_set())).await;
    held_and_released(&mut s, &workload).await;
    (h, s, project, workload, created)
}

#[tokio::test]
async fn the_node_reports_volumes_and_a_client_reads_them_under_their_project() {
    let (h, mut s, project, workload) = placed().await;

    let stranger = "wl_00000000000000000000000000000000";
    let mut deleted = ensured(&workload, "data", false, false);
    deleted.event = agent::VolumeEventKind::VOLUME_EVENT_KIND_DELETED.into();
    let records = vec![
        volume_record(1, ensured(&workload, "data", false, false)),
        volume_record(2, ensured(&workload, "stream_console", true, true)),
        volume_record(3, ensured(stranger, "data", true, false)),
    ];
    send(&mut s, records).await;
    assert_eq!(until(&mut s, ack).await, 3);
    assert_eq!(h.effects("volume.ensured").await.len(), 2);
    assert_eq!(h.effects("journal.rejected").await.len(), 1, "a stranger's volume is refused");

    let volumes = h.volumes(h.root());
    let listed = volumes.list_volumes(list(&project, false)).await.unwrap().into_owned().volumes;
    assert_eq!(listed.len(), 2);
    let data = listed.iter().find(|v| v.name == "data").unwrap();
    assert_eq!((data.quota_enforced, data.size_bytes), (false, 0), "an unenforced size is unshown");
    assert_eq!(data.attached_to.as_deref(), Some(workload.as_str()));
    assert_eq!(data.created_by, workload);
    let console = listed.iter().find(|v| v.name == "stream_console").unwrap();
    assert_eq!(console.kind.as_known(), Some(pb::VolumeKind::VOLUME_KIND_STREAM));
    assert_eq!(console.size_bytes, 1 << 30, "an enforced size is shown");

    let get = pb::GetVolumeRequest { id: data.id.clone(), ..Default::default() };
    let got = volumes.get_volume(get.clone()).await.unwrap().into_owned().volume;
    assert_eq!(got.as_option().unwrap().id, data.id);
    let (_, secret) = h.bare_token().await;
    let other = h.volumes(h.config(Some(secret.expose())));
    assert_eq!(other.get_volume(get).await.unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(
        other.list_volumes(list(&project, false)).await.unwrap_err().code,
        ErrorCode::NotFound
    );

    send(&mut s, vec![volume_record(4, deleted)]).await;
    assert_eq!(until(&mut s, ack).await, 4);
    let live = volumes.list_volumes(list(&project, false)).await.unwrap().into_owned().volumes;
    assert_eq!(live.len(), 1, "a deleted volume is not listed by default");
    let all = volumes.list_volumes(list(&project, true)).await.unwrap().into_owned().volumes;
    let gone = all.iter().find(|v| v.name == "data").unwrap();
    assert!(gone.deleted_at.is_set() && gone.attached_to.is_none(), "but kept, and marked");
}

pub(crate) fn delete(workload: &str, volumes: &[&str]) -> pb::DeleteWorkloadRequest {
    pb::DeleteWorkloadRequest {
        mutation: mutation(&format!("delete-{}", volumes.join("-"))).into(),
        workload: workload.to_owned(),
        delete_volumes: volumes.iter().map(|v| (*v).to_owned()).collect(),
        ..Default::default()
    }
}

/// A token holding `actions` on the root organization; its client configuration.
pub(crate) async fn holding(h: &Harness, actions: &[&str]) -> connectrpc::client::ClientConfig {
    let (token, secret) = h.bare_token().await;
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation(&format!("grant-{}", actions.join(","))).into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: actions.iter().map(|a| (*a).to_owned()).collect(),
            scope: org_scope(&h.root_org()).into(),
            ..Default::default()
        })
        .await
        .unwrap();
    h.config(Some(secret.expose()))
}

#[tokio::test]
async fn a_deletion_deletes_only_the_volumes_it_names_each_by_an_irreversible_step() {
    let (h, mut s, _, workload) = placed().await;
    let records = vec![
        volume_record(1, ensured(&workload, "data", true, false)),
        volume_record(2, ensured(&workload, "stream_console", true, true)),
        segment_record(3, &workload, agent::SegmentEvent::SEGMENT_EVENT_SEALED, (0, 100)),
    ];
    send(&mut s, records).await;
    assert_eq!(until(&mut s, ack).await, 3);

    let workloads = h.workloads(h.root());
    let e = workloads.delete_workload(delete(&workload, &["world"])).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument, "a name that is no volume of it is refused");
    let no_volumes = holding(&h, &["workload.read", "workload.delete", "volume.read"]).await;
    let e = h.workloads(no_volumes).delete_workload(delete(&workload, &["data"])).await;
    assert_eq!(e.unwrap_err().code, ErrorCode::PermissionDenied, "volume.delete is its own");

    let both = ["data", "stream_console"];
    let deleted = workloads.delete_workload(delete(&workload, &both)).await.unwrap();
    let plan = deleted.into_owned().plan.into_option().unwrap();
    let actions: Vec<&str> = plan.steps.iter().map(|s| s.action.as_str()).collect();
    assert_eq!(actions.last_chunk::<3>(), Some(&["retire", "delete_volume", "delete_volume"]));
    assert!(!actions.contains(&"detach_volume"), "nothing is retained");
    let gone = plan.steps.iter().filter(|s| s.action == "delete_volume");
    assert!(gone.clone().all(|s| !s.reversible), "each deletion is irreversible");
    let helper = |s: &&pb::Step| s.detail.get("helper_id").cloned();
    assert_eq!(gone.filter_map(|s| helper(&s)).count(), 2, "named as the node reported it");
    assert_ne!(plan.approval.as_known(), Some(pb::PlanApproval::PLAN_APPROVAL_AUTO), "never auto");

    let mut stream_deleted = ensured(&workload, "stream_console", true, true);
    stream_deleted.event = agent::VolumeEventKind::VOLUME_EVENT_KIND_DELETED.into();
    send(&mut s, vec![volume_record(4, stream_deleted)]).await;
    assert_eq!(until(&mut s, ack).await, 4);
    let get = pb::GetStreamRequest { workload: workload.clone(), ..Default::default() };
    let stream = h.streams(h.root()).get_stream(get).await.unwrap().into_owned();
    let lost: Vec<(u64, u64)> = stream.lost.iter().map(|r| (r.first, r.end)).collect();
    assert_eq!(lost, [(0, 100)], "its volume gone, nothing of the stream is held on the node");
}
