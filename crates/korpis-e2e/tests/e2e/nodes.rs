// SPDX-License-Identifier: AGPL-3.0-or-later

//! Two nodes on this one machine, joined to one control plane (§2 of docs/05-scheduling.md): each
//! has its own pool, ports, network namespaces, volumes, and cgroups, so neither collides with the
//! other, and the scheduler places across them. Two workloads that each must avoid a host the
//! other is on run one on each node, both converge, and a third that must avoid both finds no
//! node, the explanation naming the rule each node failed.

use crate::node::{Node, live};
use crate::recipes::{VERSION, push_sleeper};
use crate::support::{ControlPlane, converged, mutation};
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use std::collections::BTreeSet;
use std::time::Duration;

/// Declares workload `name` in `project`, labelled `app=pair`, which must not share a host with
/// another workload so labelled; its wire id.
async fn apart(cp: &ControlPlane, project: &str, name: &str, repo: &str) -> String {
    let pair = || [("app".to_owned(), "pair".to_owned())].into_iter().collect();
    let rule = pb::AffinityRule {
        anti: true,
        required: true,
        workloads: pb::LabelSelector { match_labels: pair(), ..Default::default() }.into(),
        topology_key: "host".into(),
        ..Default::default()
    };
    let placement = pb::PlacementSpec { affinity: vec![rule], ..Default::default() };
    let recipe =
        pb::RecipeRef { name: repo.to_owned(), version: VERSION.into(), ..Default::default() };
    let request = pb::DeclareIntentRequest {
        mutation: mutation(&format!("declare-{name}")).into(),
        target: Some(Target::Create(Box::new(pb::NewWorkload {
            project: project.to_owned(),
            name: name.to_owned(),
            labels: pair(),
            ..Default::default()
        }))),
        body: pb::IntentBody {
            recipe: recipe.into(),
            placement: placement.into(),
            ..Default::default()
        }
        .into(),
        ..Default::default()
    };
    let declared = Box::pin(cp.workloads().declare_intent(request)).await.unwrap().into_owned();
    declared.workload.as_option().unwrap().id.clone()
}

/// The node workload `w` is placed on.
async fn placed(cp: &ControlPlane, w: &str) -> String {
    let status = cp.status(w).await;
    status.placement.as_option().map(|p| p.node.clone()).unwrap_or_default()
}

/// Why no node took workload `w`, once the scheduler has said.
async fn unplaced(cp: &ControlPlane, w: &str) -> pb::Explanation {
    for _ in 0..300 {
        let status = cp.status(w).await;
        if let Some(why) = status.unplaced.as_option().and_then(|u| u.explanation.as_option()) {
            return why.clone();
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("{w} was never refused: {:?}", cp.status(w).await);
}

#[tokio::test]
async fn two_nodes_on_one_machine_each_run_one_of_two_workloads_that_must_stay_apart() {
    if !live() {
        return;
    }
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let (a, b) = (Node::join(&cp, "pair-a").await, Node::join(&cp, "pair-b").await);
    assert_ne!(a.pool, b.pool, "each node hands out its own addresses");
    assert_ne!(a.netns, b.netns, "and binds its own network namespaces");
    let (_on_a, _on_b) = (a.agent().await, b.agent().await);
    let repo = push_sleeper().await;

    let (mut declared, mut nodes) = (Vec::new(), BTreeSet::new());
    for name in ["left", "right"] {
        let w = apart(&cp, &project, name, &repo).await;
        a.guard(&w);
        b.guard(&w);
        converged(&cp, &w).await;
        nodes.insert(placed(&cp, &w).await);
        declared.push(w);
    }
    let fleet = pb::ListNodesRequest { organization: cp.root_org.clone(), ..Default::default() };
    let listed = cp.nodes().list_nodes(fleet).await.unwrap();
    let ids: BTreeSet<String> = listed.into_owned().nodes.iter().map(|n| n.id.clone()).collect();
    assert_eq!(nodes, ids, "one on each of the two nodes");

    let third = apart(&cp, &project, "third", &repo).await;
    let why = unplaced(&cp, &third).await;
    let refused: BTreeSet<&str> =
        why.filtered.iter().filter(|f| f.check == "affinity").map(|f| f.node.as_str()).collect();
    assert_eq!(refused, ids.iter().map(String::as_str).collect(), "each node holds one: {why:?}");
    declared.push(third);
    // The unplaced one first: retiring either placed one frees a node it would be placed on
    // between its deletion's proposal and its approval, which then refuses.
    for w in declared.iter().rev() {
        cp.retire(w).await;
    }
}
