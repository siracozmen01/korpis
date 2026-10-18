// SPDX-License-Identifier: AGPL-3.0-or-later

//! What a node reports of a run (§4.2 of docs/05-scheduling.md): believed only from the node the
//! workload is placed on, judged by the exit probe of the intent the run was made under, and
//! shown as the workload's newest run, which is converged between its runs as during one.

use crate::agent::{Stream, TestAgent, next};
use crate::assignments::{connect, set};
use crate::journal::{ack, assignment, complete, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, SystemTime};

fn text(s: &str) -> pb::Value {
    pb::Value {
        kind: Some(pb::__buffa::oneof::value::Kind::StringValue(s.to_owned())),
        ..Default::default()
    }
}

/// A node's report of `run`, journaled at `seq`, with `after` beside the run.
pub(crate) fn reported(
    seq: u64,
    workload: &str,
    action: &str,
    after: &[(&str, &str)],
) -> agent::JournalRecord {
    let mut effect = pb::Effect {
        workload: Some(workload.to_owned()),
        action: action.into(),
        outcome: pb::EffectOutcome::EFFECT_OUTCOME_SUCCEEDED.into(),
        ..Default::default()
    };
    for (k, v) in after {
        effect.after.insert((*k).to_owned(), text(v));
    }
    agent::JournalRecord {
        seq,
        body: Some(B::Effect(Box::new(effect))),
        count: 1,
        ..Default::default()
    }
}

async fn report(s: &mut Stream, record: agent::JournalRecord) {
    let seq = record.seq;
    send(s, vec![record]).await;
    assert_eq!(until(s, ack).await, seq);
}

async fn run(h: &Harness, id: &str) -> pb::Run {
    let get = pb::GetRunRequest { id: id.to_owned(), ..Default::default() };
    h.runs(h.root()).get_run(get).await.unwrap().into_owned().run.into_option().unwrap()
}

async fn status(h: &Harness, w: &str) -> pb::WorkloadStatus {
    let get = pb::GetWorkloadRequest { id: w.to_owned(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    got.status.into_option().unwrap()
}

/// A task whose exit probe counts 3 as success as well as 0.
fn lenient(repo: &str) -> pb::IntentBody {
    let exit = pb::ExitProbe { success_codes: vec![0, 3], ..Default::default() };
    let probe = pb::Probe {
        kind: Some(pb::__buffa::oneof::probe::Kind::Exit(Box::new(exit))),
        ..Default::default()
    };
    pb::IntentBody {
        lifecycle: pb::Lifecycle::LIFECYCLE_TASK.into(),
        health: pb::HealthSpec { liveness: probe.into(), ..Default::default() }.into(),
        ..body(repo, &[])
    }
}

#[tokio::test]
async fn a_run_is_believed_only_from_its_node_and_judged_by_its_intent_s_exit_probe() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "jobs").await;
    let mut sa = connect(&h, &a, 1, "").await;
    assert!(set(&next(&mut sa).await).assignments.is_empty());
    let declared = create("task", &project, "build", lenient(&repo));
    let declared = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let workload = declared.workload.into_option().unwrap().id;
    let intent = declared.intent.into_option().unwrap().id;
    let created = until(&mut sa, assignment(|x| x.plan.is_set())).await;
    let seq = complete(&mut sa, &workload, created.plan.as_option().unwrap(), 1).await;
    let id = crate::runs::made(&h, &workload).await.remove(0).id;
    let told = until(&mut sa, assignment(|x| !x.plan.is_set() && !x.runs.is_empty())).await;
    assert_eq!(told.runs[0].run, id);

    // Another node, enrolled after the task was placed, is not believed.
    let other = TestAgent::enroll(&h, "fra-4", 2).await;
    let mut so = connect(&h, &other, 2, "").await;
    report(&mut so, reported(1, &workload, "run.started", &[("run", &id)])).await;
    assert_eq!(run(&h, &id).await.status.as_known(), Some(pb::RunStatus::RUN_STATUS_PENDING));
    assert_eq!(h.effects("journal.rejected").await.len(), 1);

    report(&mut sa, reported(seq + 1, &workload, "run.started", &[("run", &id)])).await;
    let started = run(&h, &id).await;
    assert_eq!(started.status.as_known(), Some(pb::RunStatus::RUN_STATUS_RUNNING));
    assert_eq!(started.node, a.node);
    let newest = status(&h, &workload).await.newest_run.into_option().expect("its newest run");
    assert_eq!(newest.id, id);

    let ended = [("run", id.as_str()), ("cause", "exit"), ("exit_status", "3")];
    report(&mut sa, reported(seq + 2, &workload, "run.ended", &ended)).await;
    let ended = run(&h, &id).await;
    assert_eq!(ended.status.as_known(), Some(pb::RunStatus::RUN_STATUS_SUCCEEDED), "{ended:?}");
    assert_eq!(
        (ended.exit_status, ended.cause.as_known()),
        (Some(3), Some(pb::RunCause::RUN_CAUSE_EXIT))
    );

    // Between its runs, its own object stopped, the task is where it should be.
    let seen = pb::Observation {
        workload: workload.clone(),
        observed_at: korpis_wire::timestamp(SystemTime::now()).into(),
        assignment_seen: told.generation,
        intent_seen: intent,
        power_seen: pb::PowerSeen { version: 1, cycle: 0, ..Default::default() }.into(),
        state: pb::ObservedState::OBSERVED_STATE_STOPPED.into(),
        health: pb::HealthState::HEALTH_STATE_HEALTHY.into(),
        ..Default::default()
    };
    let message = agent::AgentMessage {
        message: Some(Up::Observation(Box::new(seen))),
        ..Default::default()
    };
    sa.send(message).await.unwrap();
    for _ in 0..40 {
        let now_is = status(&h, &workload).await.convergence.as_known();
        if now_is == Some(pb::Convergence::CONVERGENCE_CONVERGED) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "a task between its runs never converged: {:?}",
        status(&h, &workload).await.convergence
    );
}
