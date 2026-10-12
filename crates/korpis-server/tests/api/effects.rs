// SPDX-License-Identifier: AGPL-3.0-or-later

//! The effect log, read through what it concerns (§10 of docs/10-api.md): a workload, a plan, or
//! a node, each gated by reading that.

use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, reason};
use korpis_proto::korpis::v1 as pb;

fn actions(listed: &pb::ListEffectsResponse) -> Vec<&str> {
    listed.effects.iter().map(|e| e.action.as_str()).collect()
}

#[tokio::test]
async fn a_plans_effects_are_listed_by_the_plan() {
    let h = Harness::new().await;
    let node = h.node(None, "fra-3", &[]).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let plan = declared.plan.as_option().unwrap().id.clone();
    let workload = declared.workload.as_option().unwrap().id.clone();
    let log = h.effect_log(h.root());
    let by = |workload: Option<&str>, plan: Option<&str>, node: Option<&str>| {
        log.list_effects(pb::ListEffectsRequest {
            workload: workload.map(str::to_owned),
            plan: plan.map(str::to_owned),
            node: node.map(str::to_owned),
            ..Default::default()
        })
    };
    let of_plan = by(None, Some(&plan), None).await.unwrap().into_owned();
    assert!(!of_plan.effects.is_empty());
    assert!(of_plan.effects.iter().all(|e| e.plan.as_deref() == Some(plan.as_str())));
    assert!(actions(&of_plan).contains(&"plan.approve"));
    let of_workload = by(Some(&workload), None, None).await.unwrap().into_owned();
    assert!(actions(&of_workload).contains(&"workload.place"), "the bind concerns the workload");
    let of_node = by(None, None, Some(&node)).await.unwrap().into_owned();
    assert_eq!(actions(&of_node), ["workload.place"], "and the node it landed on");
    let both = by(Some(&workload), Some(&plan), None).await.unwrap_err();
    assert_eq!(reason(&both), "request.invalid");
}

#[tokio::test]
async fn a_plan_whose_workload_cannot_be_read_does_not_exist() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let plan = declared.plan.as_option().unwrap().id.clone();
    let (_, secret) = h.bare_token().await;
    let log = h.effect_log(h.config(Some(secret.expose())));
    let e = log
        .list_effects(pb::ListEffectsRequest { plan: Some(plan), ..Default::default() })
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "object.not_found");
}
