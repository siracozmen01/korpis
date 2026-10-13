// SPDX-License-Identifier: AGPL-3.0-or-later

//! Cancelling a plan nodes have begun (§7 of docs/03-state.md): it lands on a step boundary.
//! The plan leaves the assignment, so the node begins no further step; the step that was in
//! flight still finishes and still counts; and the plan stays cancelled.

use crate::agent::{TestAgent, welcome};
use crate::journal::{ack, assignment, plan_status, send, step_done, until};
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation};
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

async fn completed(h: &Harness, plan: &str) -> Option<u32> {
    let request = pb::GetPlanRequest { id: plan.to_owned(), ..Default::default() };
    let got = h.plans(h.root()).get_plan(request).await.unwrap().into_owned();
    got.plan.into_option().unwrap().completed
}

#[tokio::test]
async fn a_begun_plan_is_cancelled_on_a_step_boundary() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    until(&mut s, ack).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    let carried = until(&mut s, assignment(|a| a.plan.is_set())).await;
    let plan = carried.plan.as_option().unwrap().clone();
    assert!(plan.steps.len() >= 3, "a plan with steps left to cancel: {:?}", plan.steps.len());
    let (first_step, second_step) = (plan.steps[0].index, plan.steps[1].index);

    send(&mut s, vec![step_done(1, &workload, &plan.plan, first_step)]).await;
    assert_eq!(until(&mut s, ack).await, 1);
    let cancel = pb::CancelPlanRequest {
        mutation: mutation("c").into(),
        plan: plan.plan.clone(),
        ..Default::default()
    };
    h.plans(h.root()).cancel_plan(cancel).await.unwrap();
    assert_eq!(plan_status(&h, &plan.plan).await, Some(pb::PlanStatus::PLAN_STATUS_CANCELLED));
    until(&mut s, assignment(|a| a.workload == workload && !a.plan.is_set())).await;
    assert_eq!(completed(&h, &plan.plan).await, Some(first_step));

    // The step that was in flight finishes, and counts; nothing else moves.
    send(&mut s, vec![step_done(2, &workload, &plan.plan, second_step)]).await;
    assert_eq!(until(&mut s, ack).await, 2);
    assert_eq!(completed(&h, &plan.plan).await, Some(second_step));
    assert_eq!(plan_status(&h, &plan.plan).await, Some(pb::PlanStatus::PLAN_STATUS_CANCELLED));
}
