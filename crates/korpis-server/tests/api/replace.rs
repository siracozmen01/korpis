// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload whose bound node's driver refuses it after the bind, before anything of it was made
//! there, is placed again with that node left out, its plan still open to run where it lands
//! (§2.1 of docs/05-scheduling.md). An operator's choice of node is not overruled: there, the
//! refusal fails the plan as any failure does.

use crate::agent::{Stream, TestAgent, welcome};
use crate::journal::{ack, assignment, plan_status, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, Instant, SystemTime};

/// Node `name`, enrolled with its session open; its wire id and the session.
async fn node(h: &Harness, name: &str, seed: u8) -> (String, Stream) {
    let a = TestAgent::enroll(h, name, seed).await;
    let (s, first) = TestAgent::open(h, a.hello_at(SystemTime::now(), &[seed; 16])).await;
    welcome(&first.unwrap());
    (a.node.clone(), s)
}

/// The node refusing step `step` of `plan` before anything was made.
fn refused(workload: &str, plan: &str, step: u32) -> agent::JournalRecord {
    let error = pb::Error {
        reason: "step.refused".into(),
        message: "the driver refused the target: no such seccomp profile".into(),
        ..Default::default()
    };
    let effect = pb::Effect {
        workload: Some(workload.to_owned()),
        plan: Some(plan.to_owned()),
        step: Some(step),
        action: "step.pull_recipe".into(),
        outcome: pb::EffectOutcome::EFFECT_OUTCOME_FAILED.into(),
        error: error.into(),
        ..Default::default()
    };
    agent::JournalRecord {
        seq: 1,
        body: Some(B::Effect(Box::new(effect))),
        count: 1,
        ..Default::default()
    }
}

async fn status(h: &Harness, w: &str) -> pb::WorkloadStatus {
    let get = pb::GetWorkloadRequest { id: w.to_owned(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    got.status.into_option().unwrap()
}

/// The status once `done` holds; panics after five seconds.
async fn until_status(
    h: &Harness,
    w: &str,
    done: impl Fn(&pb::WorkloadStatus) -> bool,
) -> pb::WorkloadStatus {
    let start = Instant::now();
    loop {
        let s = status(h, w).await;
        if done(&s) {
            return s;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "never happened: {s:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Node `s` refuses the first step of `w`'s plan, once it is told it.
async fn refuse(s: &mut Stream, w: &str) {
    let told = until(s, assignment(|a| a.plan.is_set())).await;
    send(s, vec![refused(w, &told.plan.as_option().unwrap().plan, 0)]).await;
    until(s, ack).await;
}

#[tokio::test]
async fn a_refusal_before_anything_was_made_places_the_workload_elsewhere_with_its_plan() {
    let h = Harness::new().await;
    // Scored alike, the first enrolled is preferred: its identifier is the lower.
    let (refusing, mut first) = node(&h, "fra-1", 1).await;
    let (other, mut second) = node(&h, "fra-2", 2).await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = create("d1", &project, "mc", body(&push_paper().await, &[]));
    let declared = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let w = declared.workload.as_option().unwrap().id.clone();
    let plan = declared.plan.as_option().unwrap().id.clone();
    assert_eq!(status(&h, &w).await.placement.as_option().unwrap().node, refusing);

    refuse(&mut first, &w).await;
    let s =
        until_status(&h, &w, |s| s.placement.as_option().is_some_and(|p| p.node == other)).await;
    let why = s.placement.as_option().unwrap().explanation.as_option().unwrap();
    let left_out = why.filtered.iter().find(|r| r.node == refusing).expect("the node left out");
    assert_eq!(left_out.check, "prepare");
    assert!(left_out.detail.contains("no such seccomp profile"), "{}", left_out.detail);
    let open = plan_status(&h, &plan).await;
    assert_ne!(open, Some(pb::PlanStatus::PLAN_STATUS_FAILED), "the plan stays open: {open:?}");
    let moved = until(&mut second, assignment(|a| a.plan.is_set())).await;
    assert_eq!(moved.plan.as_option().unwrap().plan, plan, "and runs where the workload now is");
}

#[tokio::test]
async fn an_operator_s_choice_of_node_is_not_overruled_by_a_refusal() {
    let h = Harness::new().await;
    let (chosen, mut s) = node(&h, "fra-1", 1).await;
    let cordon = pb::CordonNodeRequest {
        mutation: mutation("c").into(),
        node: chosen.clone(),
        ..Default::default()
    };
    h.nodes(h.root()).cordon_node(cordon).await.unwrap();
    let project = project(&h, &h.root_org(), "games").await;
    let declared = create("d1", &project, "mc", body(&push_paper().await, &[]));
    let declared = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let w = declared.workload.as_option().unwrap().id.clone();
    let plan = declared.plan.as_option().unwrap().id.clone();
    let pin = pb::PlaceWorkloadRequest {
        mutation: mutation("p").into(),
        workload: w.clone(),
        node: chosen.clone(),
        reason: "the spare, on purpose".into(),
        ..Default::default()
    };
    h.workloads(h.root()).place_workload(pin).await.unwrap();

    refuse(&mut s, &w).await;
    for _ in 0..50 {
        if plan_status(&h, &plan).await == Some(pb::PlanStatus::PLAN_STATUS_FAILED) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(plan_status(&h, &plan).await, Some(pb::PlanStatus::PLAN_STATUS_FAILED));
    assert_eq!(status(&h, &w).await.placement.as_option().unwrap().node, chosen, "it stays");
}
