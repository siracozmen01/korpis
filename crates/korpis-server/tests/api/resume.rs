// SPDX-License-Identifier: AGPL-3.0-or-later

//! A failed plan's unfinished steps are planned again (§3.3 of docs/01-model.md, Finding 51 of
//! docs/23-walkthroughs.md): the next plan is computed from the last intent the workload reached,
//! not from the target a failed plan never reached, so re-declaring after a failed install plans
//! the install and everything after it again, and the node resumes the install where it failed.

use crate::agent::{TestAgent, welcome};
use crate::journal::{ack, assignment, plan_status, send, step_done, until};
use crate::recipes::{body, change, create, project, push_paper};
use crate::support::Harness;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

fn actions(plan: &pb::Plan) -> Vec<&str> {
    plan.steps.iter().map(|s| s.action.as_str()).collect()
}

fn step_failed(seq: u64, workload: &str, plan: &str, step: u32) -> agent::JournalRecord {
    let effect = pb::Effect {
        workload: Some(workload.to_owned()),
        plan: Some(plan.to_owned()),
        step: Some(step),
        action: "plan.step".into(),
        outcome: pb::EffectOutcome::EFFECT_OUTCOME_FAILED.into(),
        ..Default::default()
    };
    agent::JournalRecord {
        seq,
        body: Some(B::Effect(Box::new(effect))),
        count: 1,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_create_that_failed_at_its_install_is_planned_again_from_where_it_stopped() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(create("d1", &project, "mc", body(&repo, &[])));
    let declared = declared.await.unwrap().into_owned();
    let w = declared.workload.as_option().unwrap().clone();
    let plan = declared.plan.as_option().unwrap().clone();
    let all = actions(&plan);
    assert!(all.contains(&"install") && all.contains(&"create"), "{all:?}");

    // The node completes every step before the install, and the install fails.
    let told = until(&mut s, assignment(|a| a.plan.is_set())).await;
    let node_plan = told.plan.as_option().unwrap();
    let install = plan.steps.iter().find(|s| s.action == "install").unwrap().index;
    let mut records: Vec<_> =
        (0..install).map(|i| step_done(u64::from(i) + 1, &w.id, &node_plan.plan, i)).collect();
    records.push(step_failed(u64::from(install) + 1, &w.id, &node_plan.plan, install));
    send(&mut s, records).await;
    until(&mut s, ack).await;
    for _ in 0..50 {
        if plan_status(&h, &plan.id).await == Some(pb::PlanStatus::PLAN_STATUS_FAILED) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(plan_status(&h, &plan.id).await, Some(pb::PlanStatus::PLAN_STATUS_FAILED));

    // Declaring the same body again plans what the failed plan did not finish.
    let again = workloads.declare_intent(change("d2", &w.id, 1, body(&repo, &[]))).await.unwrap();
    let again = again.into_owned().plan.into_option().unwrap();
    assert_eq!(actions(&again), all, "computed from what was reached: nothing yet");
    let detail =
        |p: &pb::Plan| p.steps.iter().find(|s| s.action == "install").unwrap().detail.clone();
    assert_eq!(
        detail(&again),
        detail(&plan),
        "the same install, which the node resumes by its digest"
    );
}
