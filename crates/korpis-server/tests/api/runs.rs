// SPDX-License-Identifier: AGPL-3.0-or-later

//! Workloads that have runs (§4 of docs/05-scheduling.md): a schedule is read in a zone the
//! control plane's database knows, a policy is stored as it applies with the defaults written
//! out, and a policy on a workload that has no runs is refused. A task's first run is made once
//! it is declared, read by whoever may read the workload, and cancelled only by whoever may stop
//! it, once.

use crate::recipes::{body, create, project, push_paper};
use crate::refusal::{detail, reason};
use crate::support::{Harness, mutation};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

fn scheduled(repo: &str, zone: &str) -> pb::IntentBody {
    pb::IntentBody {
        lifecycle: pb::Lifecycle::LIFECYCLE_SCHEDULED.into(),
        schedule: "0 3 * * *".into(),
        timezone: zone.to_owned(),
        ..body(repo, &[])
    }
}

#[tokio::test]
async fn a_scheduled_workload_is_read_in_a_known_zone_and_stores_its_policy_as_it_applies() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "jobs").await;
    let workloads = h.workloads(h.root());

    let declared = create("backup", &project, "backup", scheduled(&repo, "Europe/Berlin"));
    let declared = workloads.declare_intent(declared).await.unwrap().into_owned();
    let stored = declared.intent.into_option().unwrap().body.into_option().unwrap();
    assert_eq!(stored.timezone, "Europe/Berlin");
    let runs = stored.runs.into_option().expect("the policy, written out");
    assert_eq!(runs.concurrency.as_known(), Some(pb::Concurrency::CONCURRENCY_FORBID));
    assert_eq!((runs.keep_successful, runs.keep_failed), (Some(3), Some(10)));

    let atlantis = create("atlantis", &project, "atlantis", scheduled(&repo, "Nowhere/Atlantis"));
    let refused = workloads.declare_intent(atlantis).await.unwrap_err();
    assert_eq!(detail(&refused).field, "body.timezone", "{refused:?}");

    let persistent = pb::IntentBody { runs: pb::RunPolicy::default().into(), ..body(&repo, &[]) };
    let refused = workloads.declare_intent(create("p", &project, "p", persistent)).await;
    assert_eq!(detail(&refused.unwrap_err()).field, "body.runs");
}

/// The workload's runs, once the run maker has made at least one.
async fn made(h: &Harness, workload: &str) -> Vec<pb::Run> {
    for _ in 0..150 {
        let list = pb::ListRunsRequest { workload: workload.to_owned(), ..Default::default() };
        let runs = h.runs(h.root()).list_runs(list).await.unwrap().into_owned().runs;
        if !runs.is_empty() {
            return runs;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    panic!("no run was made within thirty seconds");
}

fn cancel(key: &str, run: &str) -> pb::CancelRunRequest {
    pb::CancelRunRequest {
        mutation: mutation(key).into(),
        run: run.to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_task_s_run_is_made_read_by_its_readers_and_cancelled_once_by_who_may_stop_it() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "jobs").await;
    let task =
        pb::IntentBody { lifecycle: pb::Lifecycle::LIFECYCLE_TASK.into(), ..body(&repo, &[]) };
    let declared = create("task", &project, "build", task);
    let declared = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let workload = declared.workload.into_option().unwrap().id;

    let runs = made(&h, &workload).await;
    assert_eq!(runs.len(), 1, "one run for cycle zero");
    let run = &runs[0];
    assert_eq!(run.trigger.as_known(), Some(pb::RunTrigger::RUN_TRIGGER_MANUAL));
    assert_eq!(run.status.as_known(), Some(pb::RunStatus::RUN_STATUS_PENDING));
    assert_eq!(run.cycle, Some(0));
    let get = pb::GetRunRequest { id: run.id.clone(), ..Default::default() };
    let got = h.runs(h.root()).get_run(get.clone()).await.unwrap().into_owned();
    assert_eq!(got.run.into_option().unwrap().id, run.id);

    let (reader, secret) = h.bare_token().await;
    h.issue_to(reader, "read", &["workload.read"]).await;
    let as_reader = h.runs(h.config(Some(secret.expose())));
    assert!(as_reader.get_run(get).await.is_ok(), "a reader reads it");
    let refused = as_reader.cancel_run(cancel("r1", &run.id)).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied, "{refused:?}");

    let cancelled = h.runs(h.root()).cancel_run(cancel("c1", &run.id)).await.unwrap().into_owned();
    let cancelled = cancelled.run.into_option().unwrap();
    assert_eq!(cancelled.status.as_known(), Some(pb::RunStatus::RUN_STATUS_CANCELLED));
    assert_eq!(cancelled.cause.as_known(), Some(pb::RunCause::RUN_CAUSE_CANCELLED));
    let again = h.runs(h.root()).cancel_run(cancel("c2", &run.id)).await.unwrap_err();
    assert_eq!(reason(&again), "run.ended", "{again:?}");
    assert_eq!(h.effects("run.cancelled").await.len(), 1);
}

#[tokio::test]
async fn the_node_a_task_is_placed_on_is_told_its_run_with_what_it_enforces() {
    use crate::agent::{TestAgent, next};
    use crate::assignments::{connect, only, set};
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "jobs").await;
    let timeout = pb::RunPolicy {
        timeout: korpis_wire::duration(std::time::Duration::from_secs(600)).into(),
        ..Default::default()
    };
    let task = pb::IntentBody {
        lifecycle: pb::Lifecycle::LIFECYCLE_TASK.into(),
        runs: timeout.into(),
        ..body(&repo, &[])
    };
    let mut stream = connect(&h, &a, 1, "").await;
    assert!(set(&next(&mut stream).await).assignments.is_empty());
    let declared = create("task", &project, "build", task);
    let declared = h.workloads(h.root()).declare_intent(declared).await.unwrap().into_owned();
    let workload = declared.workload.into_option().unwrap().id;
    // The sets sent meanwhile wait on the stream, in order.
    let run = made(&h, &workload).await.remove(0);
    let mut told = only(&a, &set(&next(&mut stream).await));
    while told.runs.is_empty() {
        told = only(&a, &set(&next(&mut stream).await));
    }
    let [named] = told.runs.as_slice() else { panic!("one run named, got {:?}", told.runs) };
    assert_eq!((named.run.as_str(), named.attempt), (run.id.as_str(), 1));
    assert_eq!(named.trigger.as_known(), Some(pb::RunTrigger::RUN_TRIGGER_MANUAL));
    let enforced = named.timeout.as_option().map(|d| d.seconds);
    assert_eq!(enforced, Some(600), "the node enforces the timeout");

    // Cancelled, it is named no more: the node stops it.
    let cancelled = h.runs(h.root()).cancel_run(cancel("c", &run.id)).await.unwrap();
    assert!(cancelled.into_owned().run.is_set());
    let told = only(&a, &set(&next(&mut stream).await));
    assert!(told.runs.is_empty(), "{:?}", told.runs);
}
