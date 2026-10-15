// SPDX-License-Identifier: AGPL-3.0-or-later

//! Churn (§2.2 of docs/05-scheduling.md): a node the sweep marks unreachable while it holds a
//! workload has a loss recorded, once however many it held, and a node that held nothing has
//! none; a node's recent losses lower its churn score in every later placement.

use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use korpis_proto::korpis::v1 as pb;
use std::time::Duration;

/// Declares workload `name`; the placement it was given.
async fn placed(h: &Harness, project: &str, repo: &str, name: &str) -> pb::Placement {
    let request = create(&format!("d-{name}"), project, name, body(repo, &[]));
    let out = h.workloads(h.root()).declare_intent(request).await.unwrap().into_owned();
    let get = pb::GetWorkloadRequest {
        id: out.workload.as_option().unwrap().id.clone(),
        ..Default::default()
    };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    got.status.into_option().unwrap().placement.into_option().unwrap()
}

async fn scalar(h: &Harness, sql: &'static str) -> i64 {
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar(sql).fetch_one(store.pool()).await.unwrap()
}

#[tokio::test]
async fn a_node_lost_while_holding_a_workload_scores_lower_for_a_day() {
    // Nodes no agent heartbeats are swept as silent within a second.
    let h = Harness::with_heartbeat(Duration::from_millis(300)).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let holding = h.node(None, "fra-1", &[]).await;
    let empty = h.node(None, "fra-2", &[]).await;
    let first = placed(&h, &project, &repo, "mc").await;
    assert_eq!(first.node, holding, "scored alike, the lower identifier");

    for _ in 0..50 {
        if scalar(&h, "SELECT count(*) FROM node WHERE connectivity = 'unreachable'").await == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let losses = "SELECT count(*) FROM node_loss l JOIN node n ON n.id = l.node_id
                   WHERE n.name = 'fra-1' AND l.workloads = 1";
    assert_eq!(scalar(&h, losses).await, 1, "one loss, for the node that held the workload");
    assert_eq!(scalar(&h, "SELECT count(*) FROM node_loss").await, 1, "none for the empty node");

    // Both heard again: the one that lost its workload scores lower.
    let store = h.db.store().await.unwrap();
    let heard = "UPDATE node SET connectivity = 'connected', last_seen = now() + interval '1 hour'";
    sqlx::query(heard).execute(store.pool()).await.unwrap();
    let second = placed(&h, &project, &repo, "mc2").await;
    let why = second.explanation.as_option().unwrap();
    let churn = |n: &str| {
        why.scored.iter().find(|s| s.node == n && s.signal == "churn").map(|s| s.value).unwrap()
    };
    assert!((churn(&holding) - 0.5).abs() < 0.01, "one fresh loss: {}", churn(&holding));
    assert!((churn(&empty) - 1.0).abs() < 1e-9);
    assert_eq!(second.node, empty);
}
