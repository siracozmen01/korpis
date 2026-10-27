// SPDX-License-Identifier: AGPL-3.0-or-later

//! Replicated volumes as declared (§2.2 of docs/06-storage.md): a volume asking for replicas is
//! refused while no node's class can send increments, and once one can it is placed there and
//! not on a node whose class cannot; its replicas, interval, and the workload's
//! `replica_failover` are kept as declared.

use crate::agent::{TestAgent, welcome};
use crate::recipes::{body, create, project, push_paper};
use crate::refusal::detail;
use crate::support::Harness;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

/// A node's default class named `local`, able to send increments when `sends`, as btrfs is.
fn local(sends: bool) -> Vec<pb::StorageClassInfo> {
    let backend = if sends {
        pb::StorageBackend::STORAGE_BACKEND_BTRFS
    } else {
        pb::StorageBackend::STORAGE_BACKEND_DIRECTORY
    };
    vec![pb::StorageClassInfo {
        name: "local".into(),
        backend: backend.into(),
        capabilities: pb::StorageCapabilities {
            snapshot: sends,
            incremental_send: sends,
            fencing: pb::Fencing::FENCING_NONE.into(),
            ..Default::default()
        }
        .into(),
        capacity_bytes: 1 << 40,
        free_bytes: 1 << 40,
        ..Default::default()
    }]
}

/// The example's declaration with its `data` volume kept on two other nodes every two minutes,
/// failover allowed onto them.
fn replicated(repo: &str) -> pb::IntentBody {
    let mut b = body(repo, &[]);
    b.volumes = vec![pb::VolumeSpec {
        name: "data".into(),
        replicas: 2,
        replication_interval: buffa_types::google::protobuf::Duration {
            seconds: 120,
            ..Default::default()
        }
        .into(),
        ..Default::default()
    }];
    b.continuity = pb::ContinuitySpec {
        replica_failover: pb::ReplicaFailover::REPLICA_FAILOVER_AUTOMATIC.into(),
        ..Default::default()
    }
    .into();
    b
}

#[tokio::test]
async fn a_volume_with_replicas_needs_a_class_that_sends_increments() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-1", 1).await;
    let (_sa, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], local(false))).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());

    let refused = workloads.declare_intent(create("db-1", &project, "db", replicated(&repo)));
    let e = detail(&refused.await.unwrap_err());
    assert_eq!(e.field, "volumes[0].replicas", "{e:?}");

    let b = TestAgent::enroll(&h, "fra-2", 2).await;
    let (_sb, first) =
        TestAgent::open(&h, b.hello_with(SystemTime::now(), &[2; 16], local(true))).await;
    welcome(&first.unwrap());
    let declared = workloads.declare_intent(create("db-2", &project, "db", replicated(&repo)));
    let declared = declared.await.unwrap().into_owned();
    let id = declared.workload.into_option().unwrap().id;
    let get = pb::GetWorkloadRequest { id, ..Default::default() };
    let status = workloads.get_workload(get).await.unwrap().into_owned().status.into_option();
    let status = status.unwrap();
    assert_eq!(status.placement.into_option().unwrap().node, b.node, "where it can be sent");
    let kept = status.target.into_option().unwrap().body.into_option().unwrap();
    let volume = kept.volumes.iter().find(|v| v.name == "data").unwrap();
    let interval = volume.replication_interval.as_option().map(|d| d.seconds);
    assert_eq!((volume.replicas, interval), (2, Some(120)), "as declared");
    let continuity = kept.continuity.into_option().unwrap();
    let automatic = Some(pb::ReplicaFailover::REPLICA_FAILOVER_AUTOMATIC);
    assert_eq!(continuity.replica_failover.as_known(), automatic);
}
