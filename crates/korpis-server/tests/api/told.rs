// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node is told (§4.8 of docs/02-architecture.md, Finding 52 of docs/23-walkthroughs.md):
//! what the workload's plans reached, or what its active plan is reaching, never a target no plan
//! has made. A create approved for a later window, and a create whose plan failed, are told with
//! nothing to run; the node keeps the assignment, and with it what the plan's steps saved.

use crate::agent::{Stream, TestAgent, welcome};
use crate::journal::{ack, assignment, send, step_done, until};
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::timestamp;
use std::time::{Duration, SystemTime};

/// Whether `a` gives the node nothing to run.
fn nothing_to_run(a: &agent::Assignment) -> bool {
    let power = a.power.as_option().unwrap();
    power.desired.as_known() == Some(pb::PowerState::POWER_STATE_STOPPED) && power.version == 0
}

async fn session(h: &Harness) -> Stream {
    let a = TestAgent::enroll(h, "fra-1", 1).await;
    let (s, first) = TestAgent::open(h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    s
}

/// Declares workload `name` for approval; it and its plan.
async fn proposed(h: &Harness, name: &str) -> (String, String) {
    let project = project(h, &h.root_org(), "games").await;
    let mut declare = create("d1", &project, name, body(&push_paper().await, &[]));
    declare.propose = true;
    let out = h.workloads(h.root()).declare_intent(declare).await.unwrap().into_owned();
    (out.workload.as_option().unwrap().id.clone(), out.plan.as_option().unwrap().id.clone())
}

#[tokio::test]
async fn a_create_approved_for_later_is_told_with_nothing_to_run_until_its_window() {
    let h = Harness::new().await;
    let mut s = session(&h).await;
    let (_, plan) = proposed(&h, "mc").await;
    let approve = pb::ApprovePlanRequest {
        mutation: mutation("a").into(),
        plan: plan.clone(),
        apply_after: timestamp(SystemTime::now() + Duration::from_secs(2)).into(),
        ..Default::default()
    };
    h.plans(h.root()).approve_plan(approve).await.unwrap();
    let early = until(&mut s, assignment(|_| true)).await;
    assert!(nothing_to_run(&early), "{:?}", early.power);
    assert!(!early.plan.is_set());
    let due = until(&mut s, assignment(|a| a.plan.is_set())).await;
    assert_eq!(due.plan.as_option().unwrap().plan, plan);
    let power = due.power.as_option().unwrap();
    assert_eq!(power.desired.as_known(), Some(pb::PowerState::POWER_STATE_RUNNING));
}

fn step_failed(workload: &str, plan: &str, step: u32) -> agent::JournalRecord {
    let effect = pb::Effect {
        workload: Some(workload.to_owned()),
        plan: Some(plan.to_owned()),
        step: Some(step),
        action: "step.install".into(),
        outcome: pb::EffectOutcome::EFFECT_OUTCOME_FAILED.into(),
        ..Default::default()
    };
    let body = Some(B::Effect(Box::new(effect)));
    agent::JournalRecord { seq: u64::from(step) + 1, body, count: 1, ..Default::default() }
}

#[tokio::test]
async fn a_create_whose_plan_failed_is_told_its_target_with_nothing_to_run() {
    let h = Harness::new().await;
    let mut s = session(&h).await;
    let (w, plan) = proposed(&h, "mc").await;
    h.plans(h.root()).approve_plan(crate::lifecycle::approve("a", &plan)).await.unwrap();
    let told = until(&mut s, assignment(|a| a.plan.is_set())).await;
    let node_plan = told.plan.as_option().unwrap().clone();
    let install = node_plan.steps.iter().find(|s| s.action == "install").unwrap().index;
    let mut records: Vec<_> =
        (0..install).map(|i| step_done(u64::from(i) + 1, &w, &node_plan.plan, i)).collect();
    records.push(step_failed(&w, &node_plan.plan, install));
    send(&mut s, records).await;
    until(&mut s, ack).await;
    let after = until(&mut s, assignment(|a| !a.plan.is_set())).await;
    assert!(nothing_to_run(&after), "nothing it did not make: {:?}", after.power);
    assert_eq!(after.target.as_option().unwrap().id, told.target.as_option().unwrap().id);
}
