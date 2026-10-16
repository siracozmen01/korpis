// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deleting a workload is a retire plan (§3.3 of docs/01-model.md, §4.8 of
//! docs/02-architecture.md): confirmed by a human, retaining its volumes, and retiring the
//! identity only when nothing runs any more.

use crate::lifecycle::{approve, power, workload};
use crate::recipes::{body, change, create, project, push_paper};
use crate::support::{Harness, mutation, org_scope, reason};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::principal;

fn delete(key: &str, workload: &str) -> pb::DeleteWorkloadRequest {
    pb::DeleteWorkloadRequest {
        mutation: mutation(key).into(),
        workload: workload.to_owned(),
        ..Default::default()
    }
}

async fn retire_plan(h: &Harness, key: &str, w: &str) -> pb::Plan {
    let deleted = h.workloads(h.root()).delete_workload(delete(key, w)).await.unwrap();
    deleted.into_owned().plan.into_option().unwrap()
}

fn actions(plan: &pb::Plan) -> Vec<&str> {
    plan.steps.iter().map(|s| s.action.as_str()).collect()
}

async fn get(h: &Harness, w: &str) -> pb::WorkloadStatus {
    let workloads = h.workloads(h.root());
    let got =
        workloads.get_workload(pb::GetWorkloadRequest { id: w.to_owned(), ..Default::default() });
    got.await.unwrap().into_owned().status.into_option().unwrap()
}

#[tokio::test]
async fn a_running_workload_retires_by_a_confirmed_plan_that_keeps_its_volumes() {
    let h = Harness::new().await;
    h.node(None, "fra-3", &[]).await;
    let (w, _) = workload(&h).await;
    let plan = retire_plan(&h, "d", &w).await;
    assert_eq!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED), "never auto");
    assert_eq!(actions(&plan), ["stop", "detach_volume", "retire"]);
    assert!(plan.steps.iter().all(|s| s.action != "delete_volume"), "volumes are retained");
    let impact = plan.impact.as_option().unwrap();
    assert!(impact.cost_delta.as_option().unwrap().memory_bytes < 0, "its reservation goes");
    let approved = h.plans(h.root()).approve_plan(approve("a", &plan.id)).await.unwrap();
    let approved = approved.into_owned().plan.into_option().unwrap();
    assert_eq!(approved.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_APPROVED));
    let s = get(&h, &w).await;
    assert!(!s.workload.as_option().unwrap().deleted_at.is_set(), "its node has not removed it");
    assert!(s.placement.is_set(), "and its capacity stays held until it has");
    let e = h.workloads(h.root()).stop(power("s", &w, 1)).await.unwrap_err();
    assert_eq!(reason(&e), "workload.retiring", "nothing else is declared for it");
    let e = h.workloads(h.root()).delete_workload(delete("again", &w)).await.unwrap_err();
    assert_eq!(reason(&e), "workload.retiring");
}

#[tokio::test]
async fn a_workload_nothing_was_approved_for_retires_at_once_and_frees_its_name() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let (token, secret) = h.bare_token().await;
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("grant").into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: vec!["workload.create".into(), "workload.read".into()],
            scope: org_scope(&h.root_org()).into(),
            conditions: pb::Conditions {
                requires_approval_by: org_scope(&h.root_org()).into(),
                ..Default::default()
            }
            .into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let proposer = h.config(Some(secret.expose()));
    let declared = h
        .workloads(proposer)
        .declare_intent(create("d", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let w = declared.workload.as_option().unwrap().id.clone();
    let create_plan = declared.plan.as_option().unwrap().id.clone();
    let plan = retire_plan(&h, "x", &w).await;
    assert_eq!(actions(&plan), ["retire"]);
    assert_eq!(
        plan.steps[0].executor.as_known(),
        Some(pb::StepExecutor::STEP_EXECUTOR_CONTROL_PLANE)
    );
    let done = h.plans(h.root()).approve_plan(approve("a", &plan.id)).await.unwrap();
    let done = done.into_owned().plan.into_option().unwrap();
    assert_eq!(done.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_APPLIED));
    assert!(get(&h, &w).await.workload.as_option().unwrap().deleted_at.is_set());
    let plans = h.plans(h.root());
    let stale = plans.get_plan(pb::GetPlanRequest { id: create_plan, ..Default::default() });
    let stale = stale.await.unwrap().into_owned().plan.into_option().unwrap();
    assert_eq!(stale.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_SUPERSEDED));
    let workloads = h.workloads(h.root());
    let again = workloads.declare_intent(create("n", &project, "mc", body(&repo, &[])));
    assert!(again.await.is_ok(), "the name is free again");
    assert_eq!(h.effects("workload.retire").await.len(), 1);
}

#[tokio::test]
async fn a_retirement_is_cancelled_while_no_step_has_run_and_superseded_if_the_workload_moves() {
    let h = Harness::new().await;
    h.node(None, "fra-3", &[]).await;
    let (w, repo) = workload(&h).await;
    let plan = retire_plan(&h, "d", &w).await;
    h.plans(h.root()).approve_plan(approve("a", &plan.id)).await.unwrap();
    let cancel = pb::CancelPlanRequest {
        mutation: mutation("c").into(),
        plan: plan.id.clone(),
        ..Default::default()
    };
    h.plans(h.root()).cancel_plan(cancel).await.unwrap();
    let changed = change("m", &w, 1, body(&repo, &[("motd", crate::recipes::text("hi"))]));
    h.workloads(h.root()).declare_intent(changed).await.unwrap();
    let pending = retire_plan(&h, "d2", &w).await;
    let changed = change("m2", &w, 2, body(&repo, &[("motd", crate::recipes::text("yo"))]));
    let declared = h.workloads(h.root()).declare_intent(changed).await.unwrap().into_owned();
    let p = declared.plan.as_option().unwrap();
    if p.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
        h.plans(h.root()).approve_plan(approve("ap", &p.id)).await.unwrap();
    }
    let e = h.plans(h.root()).approve_plan(approve("late", &pending.id)).await.unwrap_err();
    assert_eq!(reason(&e), "plan.superseded", "computed for a target that has moved");
}

#[tokio::test]
async fn a_workload_no_node_could_take_retires_at_once_when_its_deletion_is_approved() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    let s = get(&h, &w).await;
    assert!(s.target.is_set() && !s.placement.is_set(), "approved, and placed nowhere");
    let plan = retire_plan(&h, "d", &w).await;
    assert_eq!(actions(&plan), ["retire"], "nothing for a node to stop or remove");
    let approved = h.plans(h.root()).approve_plan(approve("a", &plan.id)).await.unwrap();
    let approved = approved.into_owned().plan.into_option().unwrap();
    assert_eq!(approved.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_APPLIED));
    assert!(get(&h, &w).await.workload.as_option().unwrap().deleted_at.is_set(), "retired");
}
