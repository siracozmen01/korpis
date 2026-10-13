// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placement by hand (§2.3 of docs/05-scheduling.md): an operator's choice overrides the cordon
//! and the taints on the node they chose, records their reason, and overrides nothing else.

use crate::lifecycle::workload;
use crate::support::{Harness, detail, mutation, reason};
use korpis_proto::korpis::v1 as pb;

fn place(key: &str, w: &str, node: &str, why: &str) -> pb::PlaceWorkloadRequest {
    pb::PlaceWorkloadRequest {
        mutation: mutation(key).into(),
        workload: w.to_owned(),
        node: node.to_owned(),
        reason: why.to_owned(),
        ..Default::default()
    }
}

async fn cordon(h: &Harness, node: &str) {
    let request = pb::CordonNodeRequest {
        mutation: mutation(&format!("cordon-{node}")).into(),
        node: node.to_owned(),
        ..Default::default()
    };
    h.nodes(h.root()).cordon_node(request).await.unwrap();
}

#[tokio::test]
async fn an_operator_places_on_a_node_they_cordoned_and_nowhere_it_cannot_run() {
    let h = Harness::new().await;
    let node = h.node(None, "fra-3", &[]).await;
    cordon(&h, &node).await;
    let (w, _) = workload(&h).await;
    let before = h.effects("workload.place").await;
    assert_eq!(before.iter().map(|(o, _)| o.as_str()).collect::<Vec<_>>(), ["failed"]);

    let e = h.workloads(h.root()).place_workload(place("p0", &w, &node, " ")).await.unwrap_err();
    assert_eq!(detail(&e).field, "reason", "the override is recorded with its reason");
    let placed = h
        .workloads(h.root())
        .place_workload(place("p1", &w, &node, "warming the cordoned spare"))
        .await
        .unwrap()
        .into_owned();
    let p = placed.placement.as_option().unwrap();
    assert_eq!(p.node, node);
    assert_eq!(p.bound_by.as_known(), Some(pb::BoundBy::BOUND_BY_OPERATOR));
    assert!(p.reserved.memory_bytes > 0, "what it holds on the node is part of the placement");
    let after = h.effects("workload.place").await;
    assert_eq!(after.last().map(|(o, g)| (o.as_str(), g.is_some())), Some(("succeeded", true)));

    let other = h.node(None, "fra-4", &[]).await;
    let e = h.workloads(h.root()).place_workload(place("p2", &w, &other, "move")).await;
    assert_eq!(e.unwrap_err().code, connectrpc::ErrorCode::Unimplemented, "a move is migration");
}

#[tokio::test]
async fn a_node_that_cannot_run_it_is_refused_whoever_chooses_it() {
    let h = Harness::new().await;
    let child = h
        .orgs(h.root())
        .create_organization(pb::CreateOrganizationRequest {
            mutation: mutation("child").into(),
            name: "reseller".into(),
            parent: Some(h.root_org()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let child = child.organization.as_option().unwrap().id.clone();
    let child = korpis_core::id::Id::parse(korpis_core::id::IdKind::Organization, &child).unwrap();
    // A node of a child organization serves that child, never the root's own projects.
    let elsewhere = h.node(Some(child.uuid()), "fra-9", &[]).await;
    let (w, _) = workload(&h).await;
    let e = h.workloads(h.root()).place_workload(place("p", &w, &elsewhere, "try")).await;
    let e = e.unwrap_err();
    assert_eq!(reason(&e), "placement.unsatisfiable", "{e:?}");
    assert!(detail(&e).message.contains("owner"), "{}", detail(&e).message);
}
