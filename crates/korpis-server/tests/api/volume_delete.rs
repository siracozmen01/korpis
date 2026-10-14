// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deleting a retained volume whose workload is gone (§7 of docs/06-storage.md): proposed under
//! `volume.delete`, refused while a workload holds it, approved by a person, and executed by its
//! node through a residual assignment that carries only the plan and leaves the set once the plan
//! is done.

use crate::journal::{ack, assignment, complete, send, until};
use crate::lifecycle::approve;
use crate::support::{Harness, mutation, reason};
use crate::volumes::{delete, ensured, holding, list, placing, volume_record};
use connectrpc::ErrorCode;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;

fn delete_volume(key: &str, id: &str) -> pb::DeleteVolumeRequest {
    pb::DeleteVolumeRequest {
        mutation: mutation(key).into(),
        id: id.to_owned(),
        ..Default::default()
    }
}

fn empty_set(d: &Down) -> Option<()> {
    matches!(d, Down::AssignmentSet(s) if s.assignments.is_empty()).then_some(())
}

/// Retires `workload`, keeping its volumes, as its node completes the plan; the next journal
/// sequence.
pub(crate) async fn retired(
    h: &Harness,
    s: &mut crate::agent::Stream,
    workload: &str,
    from: u64,
) -> u64 {
    let deleted = h.workloads(h.root()).delete_workload(delete(workload, &[])).await.unwrap();
    let plan = deleted.into_owned().plan.into_option().unwrap().id;
    h.plans(h.root()).approve_plan(approve("retire", &plan)).await.unwrap();
    let retiring = until(s, assignment(|a| a.retire.is_set())).await;
    let last = complete(s, workload, retiring.plan.as_option().unwrap(), from).await;
    until(s, empty_set).await;
    last + 1
}

#[tokio::test]
async fn a_retained_volume_is_deleted_through_a_residual_assignment() {
    let (h, mut s, project, workload, created) = placing().await;
    let last = complete(&mut s, &workload, created.plan.as_option().unwrap(), 1).await;
    send(&mut s, vec![volume_record(last + 1, ensured(&workload, "data", true, false))]).await;
    assert_eq!(until(&mut s, ack).await, last + 1);
    let volumes = h.volumes(h.root());
    let listed = volumes.list_volumes(list(&project, false)).await.unwrap().into_owned().volumes;
    let id = listed[0].id.clone();
    let e = volumes.delete_volume(delete_volume("held", &id)).await.unwrap_err();
    assert_eq!(reason(&e), "volume.attached", "a held volume goes with its workload");

    let next = retired(&h, &mut s, &workload, last + 2).await;
    let reader = holding(&h, &["volume.read"]).await;
    let e = h.volumes(reader).delete_volume(delete_volume("no", &id)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "volume.delete is needed");
    let proposed = volumes.delete_volume(delete_volume("d", &id)).await.unwrap().into_owned();
    let plan = proposed.plan.into_option().unwrap();
    assert_eq!(plan.kind.as_known(), Some(pb::PlanKind::PLAN_KIND_DELETE_VOLUME));
    assert_eq!(plan.to_volume.as_deref(), Some(id.as_str()));
    assert_eq!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED), "never auto");
    let e = volumes.delete_volume(delete_volume("again", &id)).await.unwrap_err();
    assert_eq!(reason(&e), "volume.busy", "one plan on a volume at a time");

    h.plans(h.root()).approve_plan(approve("a", &plan.id)).await.unwrap();
    let residual = until(&mut s, assignment(|a| a.residual)).await;
    assert_eq!(residual.workload, workload);
    assert_eq!(residual.lease_epoch, 0, "no lease fences it");
    let steps = residual.plan.as_option().unwrap();
    assert_eq!(steps.steps.len(), 1);
    assert_eq!(steps.steps[0].action, "delete_volume");
    let last = complete(&mut s, &workload, steps, next).await;
    until(&mut s, empty_set).await;
    let mut gone = ensured(&workload, "data", true, false);
    gone.event = agent::VolumeEventKind::VOLUME_EVENT_KIND_DELETED.into();
    send(&mut s, vec![volume_record(last + 1, gone)]).await;
    assert_eq!(until(&mut s, ack).await, last + 1, "believed after the set let go of it");
    let all = volumes.list_volumes(list(&project, true)).await.unwrap().into_owned().volumes;
    assert!(all[0].deleted_at.is_set(), "deleted, and kept as a record");
    let e = volumes.delete_volume(delete_volume("late", &id)).await.unwrap_err();
    assert_eq!(reason(&e), "volume.deleted");
}
