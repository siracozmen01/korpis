// SPDX-License-Identifier: AGPL-3.0-or-later

//! Attaching an existing volume (§7 of docs/06-storage.md): refused while another workload
//! holds it, for a console stream's volume, and for no such volume; claimed when the declaring
//! target is approved, so a third declaration is refused; and the node is told the helper's
//! identifier for it as its node reported it.

use crate::journal::{ack, assignment, complete, send, until};
use crate::lifecycle::approve;
use crate::recipes::{body, create, push_paper};
use crate::support::{Harness, reason};
use crate::volume_delete::retired;
use crate::volumes::{ensured, list, placing, volume_record};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

fn adopting(repo: &str, existing: &str) -> pb::IntentBody {
    let mut b = body(repo, &[]);
    b.volumes = vec![pb::VolumeSpec {
        name: "data".into(),
        existing: Some(existing.to_owned()),
        ..Default::default()
    }];
    b
}

async fn declare(
    h: &Harness,
    key: &str,
    (project, name): (&str, &str),
    body: pb::IntentBody,
) -> Result<pb::DeclareIntentResponse, connectrpc::ConnectError> {
    let declared = h.workloads(h.root()).declare_intent(create(key, project, name, body)).await;
    declared.map(connectrpc::UnaryResponse::into_owned)
}

#[tokio::test]
async fn an_existing_volume_is_attached_to_one_workload_at_a_time() {
    let (h, mut s, project, workload, created) = placing().await;
    let last = complete(&mut s, &workload, created.plan.as_option().unwrap(), 1).await;
    let records = vec![
        volume_record(last + 1, ensured(&workload, "data", true, false)),
        volume_record(last + 2, ensured(&workload, "stream_console", true, true)),
    ];
    send(&mut s, records).await;
    assert_eq!(until(&mut s, ack).await, last + 2);
    let listed = h.volumes(h.root()).list_volumes(list(&project, false)).await.unwrap();
    let listed = listed.into_owned().volumes;
    let id_of = |n: &str| listed.iter().find(|v| v.name == n).unwrap().id.clone();
    let (data, console) = (id_of("data"), id_of("stream_console"));
    let repo = push_paper().await;

    let refused = declare(&h, "b0", (&project, "b"), adopting(&repo, &data)).await.unwrap_err();
    assert_eq!(reason(&refused), "volume.attached", "its workload still holds it");
    let refused = declare(&h, "b1", (&project, "b"), adopting(&repo, &console)).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidArgument, "a console stream's volume is its own");
    let nothing = "vol_00000000000000000000000000000000";
    let refused = declare(&h, "b2", (&project, "b"), adopting(&repo, nothing)).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidArgument);

    retired(&h, &mut s, &workload, last + 3).await;
    let adoption = declare(&h, "b3", (&project, "b"), adopting(&repo, &data)).await.unwrap();
    let plan = adoption.plan.as_option().unwrap();
    if plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
        h.plans(h.root()).approve_plan(approve("b", &plan.id)).await.unwrap();
    }
    let adopter = adoption.workload.as_option().unwrap().id.clone();
    let told = until(&mut s, assignment(|a| a.workload == adopter && a.plan.is_set())).await;
    let helper_id = format!("{workload}.data");
    assert_eq!(told.volume_ids.get("data"), Some(&helper_id), "as its node reported it");
    let got = pb::GetVolumeRequest { id: data.clone(), ..Default::default() };
    let v = h.volumes(h.root()).get_volume(got).await.unwrap().into_owned().volume;
    let v = v.into_option().unwrap();
    assert_eq!(v.attached_to.as_deref(), Some(adopter.as_str()), "claimed at approval");
    assert_eq!(v.created_by, workload, "and still the volume its first workload made");

    let refused = declare(&h, "c", (&project, "c"), adopting(&repo, &data)).await.unwrap_err();
    assert_eq!(reason(&refused), "volume.attached", "one workload at a time");
}
