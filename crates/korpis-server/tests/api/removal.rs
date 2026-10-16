// SPDX-License-Identifier: AGPL-3.0-or-later

//! Removing a node (§3 of docs/18-operations.md, §9 of docs/17-security.md): once cordoned and
//! empty, or at once as compromised, whatever it holds. Either way it is decommissioned for good:
//! its session ends and its key opens no other. A compromised node's workloads are placed again,
//! from their targets, on the nodes that remain.

use crate::agent::{Stream, TestAgent, welcome};
use crate::recipes::{body, project, push_paper};
use crate::routes::declare;
use crate::support::{Harness, mutation, reason};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

fn remove(key: &str, node: &str, compromised: bool) -> pb::RemoveNodeRequest {
    pb::RemoveNodeRequest {
        mutation: mutation(key).into(),
        node: node.to_owned(),
        compromised,
        ..Default::default()
    }
}

/// An enrolled node, in session.
async fn joined(h: &Harness, name: &str, seed: u8) -> (TestAgent, Stream) {
    let a = TestAgent::enroll(h, name, seed).await;
    let (s, first) = TestAgent::open(h, a.hello_at(SystemTime::now(), &[seed; 16])).await;
    welcome(&first.unwrap());
    (a, s)
}

/// Whether the session's stream ends within ten seconds, whatever it is sent before.
async fn ends(s: &mut Stream) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let next = tokio::time::timeout_at(deadline, s.message::<agent::ControlMessage>());
        match next.await {
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None) | Err(_)) => return true,
            Err(_) => return false,
        }
    }
}

/// The node `workload` is placed on, once it is placed on one other than `not`.
async fn placed_away(h: &Harness, workload: &str, not: &str) -> String {
    for _ in 0..50 {
        let get = pb::GetWorkloadRequest { id: workload.to_owned(), ..Default::default() };
        let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
        let status = got.status.into_option().unwrap_or_default();
        let node = status.placement.into_option().unwrap_or_default().node;
        if !node.is_empty() && node != not {
            return node;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{workload} was not placed away from {not}");
}

#[tokio::test]
async fn a_node_is_removed_once_cordoned_and_its_key_then_opens_nothing() {
    let h = Harness::new().await;
    let (a, mut s) = joined(&h, "fra-1", 1).await;
    let nodes = h.nodes(h.root());
    let e = nodes.remove_node(remove("r1", &a.node, false)).await.unwrap_err();
    assert_eq!(reason(&e), "node.not_cordoned");

    let cordon = pb::CordonNodeRequest {
        mutation: mutation("c1").into(),
        node: a.node.clone(),
        ..Default::default()
    };
    nodes.cordon_node(cordon).await.unwrap();
    let removed = nodes.remove_node(remove("r2", &a.node, false)).await.unwrap().into_owned();
    let status = removed.node.into_option().unwrap().status.as_known();
    assert_eq!(status, Some(pb::NodeStatus::NODE_STATUS_DECOMMISSIONED));
    assert!(ends(&mut s).await, "its session ends");
    let (_, again) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[9; 16])).await;
    assert!(again.is_err(), "its key opens nothing");

    nodes.remove_node(remove("r3", &a.node, false)).await.unwrap();
    assert_eq!(h.effects("node.remove").await.len(), 1, "removed once, recorded once");
}

#[tokio::test]
async fn a_compromised_node_is_removed_whatever_it_holds_and_its_workloads_placed_again() {
    let h = Harness::new().await;
    let (lost, mut session) = joined(&h, "fra-1", 1).await;
    let project = project(&h, &h.root_org(), "games").await;
    let paper = declare(&h, &project, "paper", body(&push_paper().await, &[])).await;
    placed_away(&h, &paper.id, "").await;
    let (kept, _kept_session) = joined(&h, "fra-2", 2).await;

    let nodes = h.nodes(h.root());
    let cordon = pb::CordonNodeRequest {
        mutation: mutation("c1").into(),
        node: lost.node.clone(),
        ..Default::default()
    };
    nodes.cordon_node(cordon).await.unwrap();
    let e = nodes.remove_node(remove("r1", &lost.node, false)).await.unwrap_err();
    assert_eq!(reason(&e), "node.not_empty", "a cordoned node holding a workload stays");

    nodes.remove_node(remove("r2", &lost.node, true)).await.unwrap();
    let now = placed_away(&h, &paper.id, &lost.node).await;
    assert_eq!(now, kept.node, "placed again from its target");
    assert!(ends(&mut session).await, "its session ends");
}
