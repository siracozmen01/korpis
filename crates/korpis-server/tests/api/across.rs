// SPDX-License-Identifier: AGPL-3.0-or-later

//! A dependency across nodes (§9.1 of docs/07-networking.md, trace 74 of
//! docs/23-walkthroughs.md): a dependent whose target's node it cannot share is placed on a node
//! that reaches the target's through the mesh, is told the address the target's node gave it and
//! whether it is ready, and the target's node is told to admit the dependent's address to the
//! endpoint it depends on.

use crate::agent::{Stream, TestAgent};
use crate::dependencies::{declare, on, one_endpoint, placement};
use crate::egress::assigned;
use crate::mesh::{opened, side};
use crate::recipes::{project, push_paper};
use crate::support::{Harness, mutation};
use buffa_types::google::protobuf::Timestamp;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::dependency::Target;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `workload` observed running at `address`, its `game` endpoint ready, down `s`.
pub(crate) async fn running_at(s: &mut Stream, workload: &str, address: &str) {
    observed_at(s, (workload, address), pb::ObservedState::OBSERVED_STATE_RUNNING).await;
}

/// `workload` observed in `state` at `address`, its `game` endpoint ready, down `s`.
pub(crate) async fn observed_at(
    s: &mut Stream,
    (workload, address): (&str, &str),
    state: pb::ObservedState,
) {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    let o = pb::Observation {
        workload: workload.to_owned(),
        observed_at: Timestamp {
            seconds: i64::try_from(since.as_secs()).unwrap(),
            ..Default::default()
        }
        .into(),
        state: state.into(),
        address: address.to_owned(),
        endpoints: vec![pb::EndpointStatus {
            name: "game".into(),
            address: format!("{address}:25565"),
            ready: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    let message =
        agent::AgentMessage { message: Some(Up::Observation(Box::new(o))), ..Default::default() };
    s.send(message).await.unwrap();
}

/// `workload`'s assignment once `done` holds of it.
pub(crate) async fn assigned_once(
    h: &Harness,
    workload: &str,
    done: impl Fn(&agent::Assignment) -> bool,
) -> agent::Assignment {
    for _ in 0..100 {
        let a = assigned(h, workload).await;
        if done(&a) {
            return a;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{workload}'s assignment never came to hold");
}

#[tokio::test]
async fn a_dependent_on_another_node_is_told_its_target_s_address_and_admitted_there() {
    let h = Harness::new().await;
    let (a, b) =
        (TestAgent::enroll(&h, "dep-a", 51).await, TestAgent::enroll(&h, "dep-b", 52).await);
    // Each reports a fresh handshake with the other from its first word.
    let sa = opened(&h, &a, side(1, "10.48.0.0/22", &[2])).await;
    let sb = opened(&h, &b, side(2, "10.48.4.0/22", &[1])).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "web").await;
    let db = declare(&h, &project, "db", one_endpoint(&repo)).await;
    let db_node = placement(&h, &db.id).await.node;
    let (mut there, mut here, other) =
        if db_node == a.node { (sa, sb, b.node.clone()) } else { (sb, sa, a.node.clone()) };
    let cordon = pb::CordonNodeRequest {
        mutation: mutation("cordon-db").into(),
        node: db_node.clone(),
        ..Default::default()
    };
    h.nodes(h.root()).cordon_node(cordon).await.unwrap();

    let mut web = one_endpoint(&repo);
    web.dependencies = vec![on(Target::Workload(db.id.clone()), true)];
    let web = declare(&h, &project, "web", web).await;
    assert_eq!(placement(&h, &web.id).await.node, other, "on the node that reaches db's");

    let get = pb::GetWorkloadRequest { id: web.id.clone(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    let shown = got.status.into_option().unwrap().dependencies;
    assert_eq!(shown[0].targets[0].reach, pb::DependencyReach::DEPENDENCY_REACH_MESH);

    running_at(&mut there, &db.id, "10.48.0.2").await;
    let told = assigned_once(&h, &web.id, |a| {
        a.dependencies.first().and_then(|d| d.targets.first()).is_some_and(|t| t.ready)
    })
    .await;
    let target = &told.dependencies[0].targets[0];
    assert_eq!((target.address.as_str(), target.container_port), ("10.48.0.2", 25565));

    running_at(&mut here, &web.id, "10.48.4.2").await;
    let admits = assigned_once(&h, &db.id, |a| !a.dependents.is_empty()).await;
    let admitted = &admits.dependents[0];
    let (address, target_address) = (admitted.address.as_str(), admitted.target_address.as_str());
    assert_eq!(
        (admitted.workload.as_str(), address, target_address, admitted.container_port),
        (web.id.as_str(), "10.48.4.2", "10.48.0.2", 25565),
    );
}
