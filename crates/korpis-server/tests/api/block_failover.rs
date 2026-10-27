// SPDX-License-Identifier: AGPL-3.0-or-later

//! A block class's device through its workload's life (§2.1 of docs/06-storage.md): made and
//! attached for the lease its workload holds on one node, its volume taking the device's
//! identifier; once that node falls silent and the workload fails over, attached to the other
//! node for the new lease's epoch, the storage holding it for that epoch alone, and the status
//! saying so; and once the workload is retired and its volume deleted, detached and deleted by
//! the provider, so that nothing is left in the storage.

use crate::agent::{Stream, TestAgent, welcome};
use crate::block_storage::{Disks, fast, serving};
use crate::failover::{assignment, keep_heard};
use crate::journal::{ack, complete, send, until};
use crate::lifecycle::approve;
use crate::recipes::{body, create, project, push_paper};
use crate::streams::held;
use crate::support::Harness;
use crate::volume_delete::{delete_volume, empty_set, retired};
use crate::volumes::{ensured, list, volume_record};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

/// Renews `workload`'s lease on `node`'s stream until the storage holds its device attached
/// there for that lease's epoch; the epoch.
async fn attached_for(s: &mut Stream, workload: &str, disks: &Disks, node: &str) -> u64 {
    for _ in 0..100 {
        let epoch = held(s, workload).await;
        let at = disks.with(|d| d.values().find_map(|v| v.attached.clone()));
        if at == Some((node.to_owned(), epoch)) {
            return epoch;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    panic!("never attached to {node}: {:?}", disks.with(|d| d.clone()));
}

/// The example's declaration with its `data` volume in the block class `fast`.
fn on_fast(repo: &str) -> pb::IntentBody {
    let mut b = body(repo, &[]);
    b.volumes = vec![pb::VolumeSpec {
        name: "data".into(),
        storage_class: "fast".into(),
        size_bytes: 1 << 30,
        ..Default::default()
    }];
    b
}

async fn devices(h: &Harness, workload: &str) -> Vec<pb::BlockDeviceStatus> {
    let get = pb::GetWorkloadRequest { id: workload.to_owned(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    got.status.into_option().unwrap().block_devices
}

#[tokio::test]
async fn a_device_follows_its_workload_to_the_node_it_fails_over_to_and_goes_with_its_volume() {
    let h = Harness::with_heartbeat(Duration::from_millis(300)).await;
    let disks = Disks::default();
    serving(&h, disks.clone()).await;
    let (a, b) = (TestAgent::enroll(&h, "fra-1", 1).await, TestAgent::enroll(&h, "fra-2", 2).await);
    let heard_a = keep_heard(&h, "fra-1");
    let (mut sa, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], vec![fast()])).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(create("db", &project, "db", on_fast(&repo)));
    let db = declared.await.unwrap().into_owned().workload.into_option().unwrap().id;
    let told = until(&mut sa, assignment(|t| t.workload == db && t.plan.is_set())).await;
    let last = complete(&mut sa, &db, told.plan.as_option().unwrap(), 1).await;
    let first_epoch = attached_for(&mut sa, &db, &disks, &a.node).await;
    let mut reported = ensured(&db, "data", true, false);
    reported.storage_class = "fast".into();
    send(&mut sa, vec![volume_record(last + 1, reported)]).await;
    assert_eq!(until(&mut sa, ack).await, last + 1);
    let volumes = h.volumes(h.root());
    let listed = volumes.list_volumes(list(&project, false)).await.unwrap().into_owned().volumes;
    let data = listed.iter().find(|v| v.name == "data").unwrap().clone();
    let device = format!("{}.img", data.id);
    assert!(disks.with(|d| d.contains_key(&device)), "the volume is the device made for it");

    keep_heard(&h, "fra-2");
    let (mut sb, first) =
        TestAgent::open(&h, b.hello_with(SystemTime::now(), &[2; 16], vec![fast()])).await;
    welcome(&first.unwrap());
    heard_a.abort();
    drop(sa);
    let store = h.db.store().await.unwrap();
    let silent = "UPDATE node SET last_seen = now() - interval '1 hour' WHERE name = 'fra-1'";
    sqlx::query(silent).execute(store.pool()).await.unwrap();
    let moved = until(&mut sb, assignment(|t| t.workload == db && t.plan.is_set())).await;
    let last = complete(&mut sb, &db, moved.plan.as_option().unwrap(), 1).await;
    let epoch = attached_for(&mut sb, &db, &disks, &b.node).await;
    let mut reported = ensured(&db, "data", true, false);
    reported.storage_class = "fast".into();
    send(&mut sb, vec![volume_record(last + 1, reported)]).await;
    assert_eq!(until(&mut sb, ack).await, last + 1, "kept by the node it is attached to");
    assert!(epoch > first_epoch, "the new lease's epoch, which the storage fences the old by");
    let [shown] = devices(&h, &db).await.try_into().unwrap();
    assert_eq!((shown.volume.as_str(), shown.made, shown.attached), ("data", true, true));
    assert!(shown.reason.is_empty(), "{shown:?}");

    deleted(&h, &mut sb, (&db, &data.id), last + 2).await;
    for _ in 0..200 {
        if disks.with(|d| d.is_empty()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    panic!("the device outlived its volume: {:?}", disks.with(|d| d.clone()));
}

/// Retires `workload` on `s`'s node keeping its volume `volume`, then deletes the volume by a
/// plan that node executes, which then reports it deleted.
async fn deleted(h: &Harness, s: &mut Stream, (workload, volume): (&str, &str), from: u64) {
    let next = retired(h, s, workload, from).await;
    let proposed = h.volumes(h.root()).delete_volume(delete_volume("d", volume)).await.unwrap();
    let plan = proposed.into_owned().plan.into_option().unwrap();
    h.plans(h.root()).approve_plan(approve("delete-data", &plan.id)).await.unwrap();
    let residual = until(s, assignment(|t| t.residual)).await;
    let last = complete(s, workload, residual.plan.as_option().unwrap(), next).await;
    until(s, empty_set).await;
    let mut gone = ensured(workload, "data", true, false);
    gone.storage_class = "fast".into();
    gone.event = agent::VolumeEventKind::VOLUME_EVENT_KIND_DELETED.into();
    send(s, vec![volume_record(last + 1, gone)]).await;
    assert_eq!(until(s, ack).await, last + 1);
}
