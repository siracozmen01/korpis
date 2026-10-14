// SPDX-License-Identifier: AGPL-3.0-or-later

//! Volumes (§7 of docs/06-storage.md): kept from what the node journals, believed only from a
//! node that once held the workload's lease, read under `volume.read` on their project, and a
//! size shown only where the write path enforces it (Rule K-3).

use crate::agent::{TestAgent, welcome};
use crate::journal::{ack, assignment, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::streams::held_and_released;
use crate::support::Harness;
use connectrpc::ErrorCode;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

fn volume_record(seq: u64, e: agent::VolumeEvent) -> agent::JournalRecord {
    agent::JournalRecord { seq, body: Some(B::Volume(Box::new(e))), count: 1, ..Default::default() }
}

fn ensured(workload: &str, name: &str, quota_enforced: bool, stream: bool) -> agent::VolumeEvent {
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

fn list(project: &str, deleted: bool) -> pb::ListVolumesRequest {
    pb::ListVolumesRequest {
        project: project.to_owned(),
        include_deleted: deleted,
        ..Default::default()
    }
}

#[tokio::test]
async fn the_node_reports_volumes_and_a_client_reads_them_under_their_project() {
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
    until(&mut s, assignment(|a| a.plan.is_set())).await;
    held_and_released(&mut s, &workload).await;

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
