// SPDX-License-Identifier: AGPL-3.0-or-later

//! Quota that counts where workloads run (§5.1 of docs/05-scheduling.md, "Where a workload
//! runs"), on a real control plane with a node in the EU and one in the US: an entry for the EU
//! holds what runs on the EU's nodes and nothing else, a workload it has no room for is placed in
//! the US or, if it must be in the EU, waits saying why, and a workload that leaves the EU gives
//! its room back.

use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation};
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// An entry of the root organization bounding how many workloads run on the nodes labelled for
/// the EU.
async fn in_the_eu(h: &Harness, workloads: u32) -> Uuid {
    let id = Uuid::now_v7();
    let store = h.db.store().await.unwrap();
    sqlx::query(
        "INSERT INTO quota_entry (id, owner_kind, owner_id, nodes, reservations)
         VALUES ($1, 'organization', $2, '{\"match_labels\": {\"region\": \"eu\"}}',
                 jsonb_build_object('workloads', $3::int))",
    )
    .bind(id)
    .bind(h.installed.organization.id)
    .bind(i32::try_from(workloads).unwrap())
    .execute(store.pool())
    .await
    .unwrap();
    id
}

/// How many workloads entry `id` counts as running in the EU.
async fn counted(h: &Harness, id: Uuid) -> i64 {
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar(
        "SELECT coalesce((held->'reservations'->>'workloads')::bigint, 0) FROM quota_entry
          WHERE id = $1",
    )
    .bind(id)
    .fetch_one(store.pool())
    .await
    .unwrap()
}

/// Declares workload `name` in `project`, required in the EU if `eu`; its wire id.
async fn declared(h: &Harness, (project, repo): (&str, &str), name: &str, eu: bool) -> String {
    let mut b = body(repo, &[]);
    if eu {
        let mut selector = pb::LabelSelector::default();
        selector.match_labels.insert("region".into(), "eu".into());
        b.placement =
            pb::PlacementSpec { node_selector: selector.into(), ..Default::default() }.into();
    }
    let out = h.workloads(h.root()).declare_intent(create(name, project, name, b)).await.unwrap();
    out.into_owned().workload.as_option().unwrap().id.clone()
}

/// The status of workload `w` once `done` holds of it.
async fn once(
    h: &Harness,
    w: &str,
    done: impl Fn(&pb::WorkloadStatus) -> bool,
) -> pb::WorkloadStatus {
    let start = Instant::now();
    loop {
        let get = pb::GetWorkloadRequest { id: w.to_owned(), ..Default::default() };
        let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
        let s = got.status.into_option().unwrap();
        if done(&s) {
            return s;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "{w}: {:?}", s.unplaced);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn node(s: &pb::WorkloadStatus) -> String {
    s.placement.as_option().map(|p| p.node.clone()).unwrap_or_default()
}

#[tokio::test]
async fn an_entry_for_the_eu_holds_what_runs_there_and_sends_the_rest_elsewhere() {
    let h = Harness::new().await;
    let eu = h.node(None, "fra-1", &[("region", "eu")]).await;
    let us = h.node(None, "nyc-1", &[("region", "us")]).await;
    let e = in_the_eu(&h, 1).await;
    let (project, repo) = (project(&h, &h.root_org(), "games").await, push_paper().await);
    let at = (project.as_str(), repo.as_str());

    let first = declared(&h, at, "first", true).await;
    assert_eq!(node(&once(&h, &first, |s| s.placement.is_set()).await), eu);
    assert_eq!(counted(&h, e).await, 1, "held by the placement, in its transaction");
    let anywhere = declared(&h, at, "anywhere", false).await;
    assert_eq!(node(&once(&h, &anywhere, |s| s.placement.is_set()).await), us, "no room left");
    let waiting = declared(&h, at, "waiting", true).await;
    let s = once(&h, &waiting, |s| s.unplaced.is_set()).await;
    let why = s.unplaced.as_option().unwrap().explanation.as_option().unwrap().clone();
    let quota = why.filtered.iter().find(|f| f.node == eu).unwrap();
    assert_eq!(quota.check, "quota", "{why:?}");
    assert_eq!(counted(&h, e).await, 1, "what the US runs is not the EU's");

    // Its node lost, the workload placed there leaves the EU and gives back its room, which a
    // node new to the EU then lets exactly one of the two use.
    let nodes = h.nodes(h.root());
    let cordon = pb::CordonNodeRequest {
        mutation: mutation("c").into(),
        node: eu.clone(),
        ..Default::default()
    };
    nodes.cordon_node(cordon).await.unwrap();
    let remove = pb::RemoveNodeRequest {
        mutation: mutation("r").into(),
        node: eu,
        compromised: true,
        ..Default::default()
    };
    nodes.remove_node(remove).await.unwrap();
    assert_eq!(counted(&h, e).await, 0, "given back as it left");
    let fra2 = h.node(None, "fra-2", &[("region", "eu")]).await;
    let on = |s: &pb::WorkloadStatus| node(s) == fra2;
    let start = Instant::now();
    let mut there = 0;
    while there == 0 && start.elapsed() < Duration::from_secs(10) {
        tokio::time::sleep(Duration::from_millis(100)).await;
        there = usize::from(on(&once(&h, &first, |_| true).await))
            + usize::from(on(&once(&h, &waiting, |_| true).await));
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let both = [once(&h, &first, |_| true).await, once(&h, &waiting, |_| true).await];
    assert_eq!(both.iter().filter(|s| on(s)).count(), 1, "one runs there, the other waits");
    assert_eq!(counted(&h, e).await, 1);
    assert_eq!(h.api.app().reconcile_quota().await.unwrap(), 0, "and nothing drifted");
}
