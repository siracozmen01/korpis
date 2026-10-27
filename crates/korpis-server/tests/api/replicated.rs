// SPDX-License-Identifier: AGPL-3.0-or-later

//! Replicated volumes as declared (§2.2 of docs/06-storage.md): a volume asking for replicas is
//! refused while no node's class can send increments, and once one can it is placed there and
//! not on a node whose class cannot; its replicas, interval, and the workload's
//! `replica_failover` are kept as declared. Once its volume is made, the control plane chooses
//! the nodes keeping its replicas among those whose class sends increments, shows them on the
//! volume, and drops one whose node is removed.

use crate::agent::{Stream, TestAgent, welcome};
use crate::failover::{assignment, keep_heard};
use crate::journal::{ack, complete, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::refusal::detail;
use crate::streams::held;
use crate::support::Harness;
use crate::volumes::{ensured, list, volume_record};
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

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

/// The nodes keeping a replica of the project's `data` volume, once `done` holds of them.
pub(crate) async fn replicas_once(
    h: &Harness,
    project: &str,
    done: impl Fn(&[pb::VolumeReplica]) -> bool,
) -> Vec<pb::VolumeReplica> {
    for _ in 0..100 {
        let listed = h.volumes(h.root()).list_volumes(list(project, false)).await.unwrap();
        let listed = listed.into_owned().volumes;
        let replicas = listed.into_iter().find(|v| v.name == "data").map(|v| v.replicas);
        if let Some(r) = replicas.filter(|r| done(r)) {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the replicas never came to what was waited for");
}

/// A workload `db` whose `data` volume asks for two replicas, made on `fra-1`, and three more
/// nodes: two whose class sends increments, and one whose class does not. Every node is kept
/// heard while it lives.
pub(crate) struct Replicated {
    pub(crate) h: Harness,
    /// The volume's node, and its session.
    pub(crate) a: (TestAgent, Stream),
    pub(crate) project: String,
    /// The volume, as the API names it.
    pub(crate) volume: String,
    /// `fra-2`, `fra-3`, and `fra-4`, with their sessions.
    pub(crate) others: Vec<(TestAgent, Stream)>,
    heard: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Replicated {
    fn drop(&mut self) {
        self.heard.iter().for_each(tokio::task::JoinHandle::abort);
    }
}

impl Replicated {
    pub(crate) async fn new() -> Self {
        let h = Harness::with_heartbeat(Duration::from_millis(300)).await;
        let a = TestAgent::enroll(&h, "fra-1", 1).await;
        let mut heard = vec![keep_heard(&h, "fra-1")];
        let (mut sa, first) =
            TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], local(true))).await;
        welcome(&first.unwrap());
        let repo = push_paper().await;
        let project = project(&h, &h.root_org(), "games").await;
        let workloads = h.workloads(h.root());
        let declared = workloads.declare_intent(create("db", &project, "db", replicated(&repo)));
        let db = declared.await.unwrap().into_owned().workload.into_option().unwrap().id;
        let told = until(&mut sa, assignment(|t| t.workload == db && t.plan.is_set())).await;
        let last = complete(&mut sa, &db, told.plan.as_option().unwrap(), 1).await;
        held(&mut sa, &db).await;
        send(&mut sa, vec![volume_record(last + 1, ensured(&db, "data", true, false))]).await;
        assert_eq!(until(&mut sa, ack).await, last + 1);
        let listed = h.volumes(h.root()).list_volumes(list(&project, false)).await.unwrap();
        let volume = listed.into_owned().volumes.into_iter().find(|v| v.name == "data");
        let volume = volume.unwrap().id;
        let mut others = Vec::new();
        for (name, n, sends) in [("fra-2", 2, true), ("fra-3", 3, true), ("fra-4", 4, false)] {
            let node = TestAgent::enroll(&h, name, n).await;
            heard.push(keep_heard(&h, name));
            let hello = node.hello_with(SystemTime::now(), &[n; 16], local(sends));
            let (s, first) = TestAgent::open(&h, hello).await;
            welcome(&first.unwrap());
            others.push((node, s));
        }
        Self { h, a: (a, sa), project, volume, others, heard }
    }
}

#[tokio::test]
async fn a_volume_s_replicas_are_chosen_shown_and_dropped_with_their_node() {
    let r = Box::pin(Replicated::new()).await;
    let (h, ids) = (&r.h, [r.others[0].0.node.clone(), r.others[1].0.node.clone()]);
    let two = replicas_once(h, &r.project, |r| r.len() == 2).await;
    let mut on: Vec<&str> = two.iter().map(|r| r.node.as_str()).collect();
    on.sort_unstable();
    let mut able = [ids[0].as_str(), ids[1].as_str()];
    able.sort_unstable();
    assert_eq!(on, able, "on the other nodes whose class sends increments");
    assert!(two.iter().all(|r| r.chosen_at.is_set() && !r.received_at.is_set()), "none yet");
    assert_eq!(h.effects("volume.replica_chosen").await.len(), 2);
    let request = pb::GetNodeRequest { id: ids[0].clone(), ..Default::default() };
    let node = h.nodes(h.root()).get_node(request).await.unwrap().into_owned().node;
    let allocated = node.into_option().unwrap().allocated.into_option().unwrap_or_default();
    assert_eq!(allocated.disk_bytes, 1 << 30, "a replica takes its volume's size there");

    let removal = pb::RemoveNodeRequest {
        mutation: crate::support::mutation("remove-3").into(),
        node: ids[1].clone(),
        compromised: true,
        ..Default::default()
    };
    h.nodes(h.root()).remove_node(removal).await.unwrap();
    let one = replicas_once(h, &r.project, |r| r.len() == 1).await;
    assert_eq!(one[0].node, ids[0], "no other node able to keep one is left");
    assert_eq!(h.effects("volume.replica_dropped").await.len(), 1);
}
