// SPDX-License-Identifier: AGPL-3.0-or-later

//! The node journal (§4.10 of docs/02-architecture.md): ingested exactly once and in order,
//! acknowledged, believed only as far as the node's assignments reach; step completions apply
//! plans, and a retirement completed removes the workload.

use crate::agent::{Stream, TestAgent, next, welcome};
use crate::lifecycle::approve;
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation};
use buffa::Message as _;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

/// The next message `pick` accepts, skipping the others.
pub async fn until<T>(stream: &mut Stream, pick: impl Fn(&Down) -> Option<T>) -> T {
    for _ in 0..30 {
        if let Some(found) = next(stream).await.message.as_ref().and_then(&pick) {
            return found;
        }
    }
    panic!("not received within thirty messages");
}

pub fn ack(d: &Down) -> Option<u64> {
    if let Down::JournalAck(a) = d { Some(a.seq) } else { None }
}

/// The first assignment of the next set whose assignment satisfies `pick`.
pub fn assignment(
    pick: impl Fn(&agent::Assignment) -> bool,
) -> impl Fn(&Down) -> Option<agent::Assignment> {
    move |d| {
        let Down::AssignmentSet(s) = d else { return None };
        let a = agent::Assignment::decode_from_slice(&s.assignments.first()?.assignment).ok()?;
        pick(&a).then_some(a)
    }
}

fn empty_set(d: &Down) -> Option<()> {
    matches!(d, Down::AssignmentSet(s) if s.assignments.is_empty()).then_some(())
}

pub fn step_done(seq: u64, workload: &str, plan: &str, step: u32) -> agent::JournalRecord {
    let effect = pb::Effect {
        workload: Some(workload.to_owned()),
        plan: Some(plan.to_owned()),
        step: Some(step),
        action: "plan.step".into(),
        outcome: pb::EffectOutcome::EFFECT_OUTCOME_SUCCEEDED.into(),
        ..Default::default()
    };
    agent::JournalRecord {
        seq,
        body: Some(B::Effect(Box::new(effect))),
        count: 1,
        ..Default::default()
    }
}

pub async fn send(stream: &mut Stream, records: Vec<agent::JournalRecord>) {
    let j = agent::JournalRecords { records, ..Default::default() };
    let m = agent::AgentMessage { message: Some(Up::Journal(Box::new(j))), ..Default::default() };
    stream.send(m).await.unwrap();
}

/// Completes every step of `plan` from journal sequence `from`; the last sequence used.
pub async fn complete(s: &mut Stream, workload: &str, plan: &agent::NodePlan, from: u64) -> u64 {
    let mut seq = from;
    let mut records = Vec::new();
    for step in &plan.steps {
        records.push(step_done(seq, workload, &plan.plan, step.index));
        seq += 1;
    }
    send(s, records).await;
    assert_eq!(until(s, ack).await, seq - 1, "every completion acknowledged");
    seq - 1
}

pub async fn plan_status(h: &Harness, plan: &str) -> Option<pb::PlanStatus> {
    let got = h
        .plans(h.root())
        .get_plan(pb::GetPlanRequest { id: plan.to_owned(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    got.plan.as_option().unwrap().status.as_known()
}

async fn origin_effects(h: &Harness) -> i64 {
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar("SELECT count(*) FROM effect WHERE origin_node IS NOT NULL")
        .fetch_one(store.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn completions_apply_the_plan_once_and_a_completed_retirement_removes_the_workload() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    assert_eq!(until(&mut s, ack).await, 0, "nothing ingested yet");
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declared = h
        .workloads(h.root())
        .declare_intent(create("w", &project, "mc", body(&repo, &[])))
        .await
        .unwrap()
        .into_owned();
    let workload = declared.workload.as_option().unwrap().id.clone();
    let created = until(&mut s, assignment(|a| a.plan.is_set())).await;
    let plan = created.plan.as_option().unwrap().clone();
    assert_eq!(plan_status(&h, &plan.plan).await, Some(pb::PlanStatus::PLAN_STATUS_APPLYING));

    let last = complete(&mut s, &workload, &plan, 1).await;
    let effects = origin_effects(&h).await;
    send(&mut s, vec![step_done(1, &workload, &plan.plan, 0)]).await;
    assert_eq!(until(&mut s, ack).await, last, "a duplicate is dropped");
    send(&mut s, vec![step_done(last + 2, &workload, &plan.plan, 0)]).await;
    assert_eq!(until(&mut s, ack).await, last, "a record past a gap waits");
    assert_eq!(origin_effects(&h).await, effects, "nothing was ingested twice");
    assert_eq!(plan_status(&h, &plan.plan).await, Some(pb::PlanStatus::PLAN_STATUS_APPLIED));

    let stranger = "wl_00000000000000000000000000000000";
    send(&mut s, vec![step_done(last + 1, stranger, &plan.plan, 0)]).await;
    assert_eq!(until(&mut s, ack).await, last + 1, "a refused record still moves the cursor");
    assert_eq!(h.effects("journal.rejected").await.len(), 1, "and is kept, visibly");

    let deleted = h
        .workloads(h.root())
        .delete_workload(pb::DeleteWorkloadRequest {
            mutation: mutation("d").into(),
            workload: workload.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let retire = deleted.plan.as_option().unwrap().id.clone();
    h.plans(h.root()).approve_plan(approve("a", &retire)).await.unwrap();
    let retiring = until(&mut s, assignment(|a| a.retire.is_set())).await;
    let steps = retiring.plan.as_option().unwrap().clone();
    assert_eq!(steps.plan, retire);
    complete(&mut s, &workload, &steps, last + 2).await;
    until(&mut s, empty_set).await;
    assert_eq!(plan_status(&h, &retire).await, Some(pb::PlanStatus::PLAN_STATUS_APPLIED));
    assert_eq!(h.effects("workload.retire").await.len(), 1, "the identity is retired");
}

fn gap(seq: u64, first: u64, last: u64) -> agent::JournalRecord {
    let gap = agent::JournalGap {
        first_seq: first,
        last_seq: last,
        cause: "journal exhausted".into(),
        ..Default::default()
    };
    agent::JournalRecord { seq, body: Some(B::Gap(Box::new(gap))), count: 1, ..Default::default() }
}

#[tokio::test]
async fn a_malformed_record_is_kept_as_rejected_and_a_gap_stands_for_the_numbers_it_names() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-5", 2).await;
    let (mut s, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[2; 16])).await;
    welcome(&first.unwrap());
    assert_eq!(until(&mut s, ack).await, 0);

    send(&mut s, vec![step_done(1, "not-an-id", "nor-this", 0)]).await;
    assert_eq!(until(&mut s, ack).await, 1, "a record that does not translate still passes");
    assert_eq!(h.effects("journal.rejected").await.len(), 1, "and is kept, visibly");

    send(&mut s, vec![gap(5, 2, 5)]).await;
    assert_eq!(until(&mut s, ack).await, 5, "the gap stands for 2 through 5");
    send(&mut s, vec![gap(9, 7, 8)]).await;
    assert_eq!(until(&mut s, ack).await, 5, "a gap not numbered by its last fills nothing");
    send(&mut s, vec![gap(6, 6, 6)]).await;
    assert_eq!(until(&mut s, ack).await, 6);
    assert_eq!(h.effects("journal.gap").await.len(), 2);
}
