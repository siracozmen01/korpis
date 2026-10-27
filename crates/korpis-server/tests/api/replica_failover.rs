// SPDX-License-Identifier: AGPL-3.0-or-later

//! Failover onto a replica (§2.4 of docs/05-scheduling.md, §2.2 of docs/06-storage.md): a
//! workload declaring `replica_failover: automatic`, its `data` volume on its own node's disk
//! with two replicas, one of which received a snapshot. Its power is cut, and once its lease has
//! ended it is placed again on the node holding the freshest replica, which a lease derived as
//! `stop` lets happen (Finding 206), and made there by a remake plan that promotes the replica.
//! The volume is the same one, now on that node under a name of its own there; the copy left on
//! the node it left is a retained volume of its own, listed as divergent, which a person deletes
//! as any retained volume, through a residual assignment of the node keeping it; and the other
//! replica is marked stale, to be sent whole. The failover's effect records since when what was
//! written is lost.

use crate::agent::Stream;
use crate::failover::{assignment, kind};
use crate::journal::{ack, complete, send, until};
use crate::lifecycle::approve;
use crate::replica_transfers::{received, replicas, told_of};
use crate::replicated::{Replicated, replicas_once};
use crate::support::mutation;
use crate::volumes::{ensured, list, volume_record};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::value::Kind;

/// The snapshot `s`'s node is told it receives of the volume, once it is told one.
async fn receiving(s: &mut Stream) -> String {
    let told = told_of(s, replicas(|r| r.keeping.iter().any(|k| !k.receiving.is_empty())));
    told.await.keeping[0].receiving.clone()
}

/// The text `step`'s detail holds under `key`.
fn detail(step: &pb::Step, key: &str) -> Option<String> {
    match step.detail.get(key).and_then(|v| v.kind.as_ref()) {
        Some(Kind::StringValue(s)) => Some(s.clone()),
        _ => None,
    }
}

/// The project's `data` volume that is not a copy left behind.
async fn volume(r: &Replicated) -> pb::Volume {
    let listed = r.h.volumes(r.h.root()).list_volumes(list(&r.project, false)).await.unwrap();
    let found = listed.into_owned().volumes.into_iter();
    found.filter(|v| v.name == "data").find(|v| v.divergent_of.is_none()).unwrap()
}

/// Has the second node receive a snapshot, cuts the first's power, and waits for the workload
/// to be told to the second; the snapshot, and the assignment.
async fn failed_over(r: &mut Replicated) -> (String, agent::Assignment) {
    replicas_once(&r.h, &r.project, |r| r.len() == 2).await;
    let snapshot = receiving(&mut r.others[0].1).await;
    let sb = &mut r.others[0].1;
    send(sb, vec![received(1, &r.volume, &snapshot, false)]).await;
    until(sb, |d| ack(d).filter(|n| *n == 1)).await;
    replicas_once(&r.h, &r.project, |s| s.iter().any(|x| x.received_at.is_set())).await;
    r.cut().await;
    let db = r.db.clone();
    let sb = &mut r.others[0].1;
    (snapshot, until(sb, assignment(|t| t.workload == db && t.plan.is_set())).await)
}

#[tokio::test]
async fn a_workload_fails_over_onto_its_freshest_replica_which_becomes_its_volume() {
    let mut r = Box::pin(Replicated::new()).await;
    let (b, c) = (r.others[0].0.node.clone(), r.others[1].0.node.clone());
    let (snapshot, moved) = failed_over(&mut r).await;
    let plan = moved.plan.as_option().expect("a plan making it there");
    assert_eq!(kind(plan), Some(pb::PlanKind::PLAN_KIND_REMAKE), "the control plane's");
    let actions: Vec<&str> = plan.steps.iter().map(|s| s.action.as_str()).collect();
    assert_eq!(actions, ["pull_recipe", "promote_replica", "create", "start"], "from the replica");
    let promote = &plan.steps[1];
    assert_eq!(detail(promote, "snapshot"), Some(snapshot.clone()), "what it received");
    let record = korpis_wire::id(korpis_core::id::IdKind::Volume, "id", &r.volume).unwrap();
    assert_eq!(detail(promote, "existing"), Some(record.to_string()), "the same volume");
    let simple = uuid::Uuid::parse_str(&snapshot).unwrap().simple().to_string();
    let into = format!("{}.{simple}", r.volume);
    assert_eq!(detail(promote, "into"), Some(into.clone()), "a name of its own there");
    assert_eq!(moved.volume_ids.get("data"), Some(&into), "the node mounts it by that name");

    let v = volume(&r).await;
    assert_eq!(v.node.as_deref(), Some(b.as_str()), "the volume is now on the replica's node");
    let left: Vec<&str> = v.divergent.iter().map(|d| d.node.as_str()).collect();
    assert_eq!(left, [r.a.0.node.as_str()], "the copy left behind, listed");
    let divergent = &v.divergent[0];
    assert!(divergent.since.is_set() && divergent.held_until.is_set());
    let kept: Vec<&str> = v.replicas.iter().map(|x| x.node.as_str()).collect();
    assert_eq!(kept, [c.as_str()], "the replica it was made from is no longer one");
    assert!(v.replicas[0].stale_at.is_set(), "to be sent whole from the new node");

    let store = r.h.db.store().await.unwrap();
    let lost: Option<i64> = sqlx::query_scalar(
        "SELECT (after->>'lost_since')::bigint FROM effect WHERE action = 'workload.failover'",
    )
    .fetch_one(store.pool())
    .await
    .unwrap();
    let held_until = divergent.held_until.as_option().map(|t| t.seconds);
    assert_eq!(lost, held_until, "lost since the replica received");
}

#[tokio::test]
async fn the_copy_left_behind_is_a_retained_volume_a_person_deletes_on_its_node() {
    let mut r = Box::pin(Replicated::new()).await;
    failed_over(&mut r).await;
    let v = volume(&r).await;
    let copy = v.divergent[0].volume.clone();
    let volumes = r.h.volumes(r.h.root());
    let got = pb::GetVolumeRequest { id: copy.clone(), ..Default::default() };
    let left = volumes.get_volume(got).await.unwrap().into_owned().volume.into_option().unwrap();
    assert_eq!(left.divergent_of.as_deref(), Some(r.volume.as_str()), "of the volume");
    assert_eq!(left.node.as_deref(), Some(r.a.0.node.as_str()), "on the node it left");
    assert!(left.attached_to.is_none(), "held by no workload");

    let asked = pb::DeleteVolumeRequest {
        mutation: mutation("delete-copy").into(),
        id: copy.clone(),
        ..Default::default()
    };
    let plan = volumes.delete_volume(asked).await.unwrap().into_owned().plan.into_option();
    let plan = plan.unwrap();
    assert_eq!(plan.to_volume.as_deref(), Some(copy.as_str()));
    assert_eq!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED), "a person's");
    r.h.plans(r.h.root()).approve_plan(approve("a", &plan.id)).await.unwrap();
    let (db, next) = (r.db.clone(), r.next_a);
    let sa = &mut r.a.1;
    let residual = until(sa, assignment(|a| a.residual && a.workload == db)).await;
    let steps = residual.plan.as_option().unwrap();
    assert_eq!(steps.steps[0].action, "delete_volume", "on the node keeping the copy");
    let last = complete(sa, &db, steps, next).await;
    let mut gone = ensured(&db, "data", true, false);
    gone.event = agent::VolumeEventKind::VOLUME_EVENT_KIND_DELETED.into();
    send(sa, vec![volume_record(last + 1, gone)]).await;
    assert_eq!(until(sa, ack).await, last + 1);
    let v = volume(&r).await;
    assert!(v.divergent.is_empty(), "the copy is gone, and listed no more");
    assert!(!v.deleted_at.is_set(), "and the volume itself untouched");
}
