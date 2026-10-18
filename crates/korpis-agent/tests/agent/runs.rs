// SPDX-License-Identifier: AGPL-3.0-or-later

//! A task's runs on a node, against a real control plane with the in-memory runtime standing in
//! for a driver (§4.2 and §4.5 of docs/05-scheduling.md): the run its assignment names is started
//! once in an object of its own, its end reported with its exit status and never run again while
//! it is still named, removed once it is cancelled, and stopped at its timeout.

use crate::session::until;
use crate::support::{ControlPlane, facts, mutation, push_paper};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::declare_intent_request::Target;
use std::sync::Arc;
use std::time::Duration;

/// A node joined to `cp`; its state.
async fn joined(cp: &ControlPlane, dir: &tempfile::TempDir) -> StateDir {
    let (token, ca) = cp.token("fra-9").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    state
}

/// The agent of `state` running, its reconciler looking every 200 ms.
async fn running(state: &StateDir, runtime: &InMemory) -> Agent {
    let options = Options {
        period: Duration::from_millis(200),
        maintain_every: Duration::from_millis(500),
        ..Options::default()
    };
    let host = InMemoryHost::default();
    let agent = Agent::start(state, runtime.clone(), host, Arc::new(facts), options).unwrap();
    let mut events = agent.events();
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    agent
}

/// A node joined to `cp` and running.
async fn node(cp: &ControlPlane, dir: &tempfile::TempDir, runtime: &InMemory) -> Agent {
    running(&joined(cp, dir).await, runtime).await
}

/// Declares task `name` with `policy`; its wire identifier.
async fn task(cp: &ControlPlane, project: &str, name: &str, policy: pb::RunPolicy) -> String {
    cp.measured().await;
    let body = pb::IntentBody {
        recipe: pb::RecipeRef {
            name: push_paper().await,
            version: "1.21.4".into(),
            ..Default::default()
        }
        .into(),
        lifecycle: pb::Lifecycle::LIFECYCLE_TASK.into(),
        runs: policy.into(),
        ..Default::default()
    };
    let request = pb::DeclareIntentRequest {
        mutation: mutation(&format!("declare-{name}")).into(),
        target: Some(Target::Create(Box::new(pb::NewWorkload {
            project: project.to_owned(),
            name: name.to_owned(),
            ..Default::default()
        }))),
        body: body.into(),
        ..Default::default()
    };
    let declared = cp.workloads().declare_intent(request).await.unwrap().into_owned();
    declared.workload.into_option().unwrap().id
}

/// Waits until `sql` counts `n`, for up to thirty seconds.
async fn counted(cp: &ControlPlane, sql: &'static str, n: i64) {
    for _ in 0..300 {
        if cp.scalar::<i64>(sql).await == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(cp.scalar::<i64>(sql).await, n, "{sql}");
}

/// The objects of `workload`: their identifiers, and whether each runs.
fn objects_of(runtime: &InMemory, workload: &str) -> Vec<(String, bool)> {
    let all = runtime.objects().into_iter();
    all.filter(|(_, (w, _))| w == workload).map(|(id, (_, running))| (id, running)).collect()
}

const STARTED: &str = "SELECT count(*) FROM effect WHERE action = 'run.started'";
const ENDED: &str = "SELECT count(*) FROM effect WHERE action = 'run.ended'";

#[tokio::test]
async fn a_task_s_run_starts_once_ends_once_and_its_object_goes_when_it_is_named_no_more() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let runtime = InMemory::new();
    let _agent = node(&cp, &dir, &runtime).await;
    let w = task(&cp, &project, "build", pb::RunPolicy::default()).await;

    counted(&cp, STARTED, 1).await;
    let held = objects_of(&runtime, &w);
    let [(object, true)] = held.as_slice() else {
        panic!("one running object for the run: {:?}", runtime.objects());
    };
    let object = object.clone();
    runtime.exit(&object, 0);
    counted(&cp, ENDED, 1).await;
    let exit: Option<(String,)> = cp
        .query_one("SELECT after::text FROM effect WHERE action = 'run.ended' AND $1 <> ''", "x")
        .await;
    let exit = exit.unwrap().0;
    assert!(exit.contains("exit_status") && exit.contains("\"exit\""), "{exit}");

    // Still named, since nothing has told the control plane it ended: never run again.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(cp.scalar::<i64>(STARTED).await, 1);
    assert_eq!(objects_of(&runtime, &w), [(object, false)], "kept while it is named");

    let run: Option<(uuid::Uuid,)> =
        cp.query_one("SELECT id FROM run WHERE workload_id::text <> $1", "").await;
    let run = korpis_core::id::Id::from_uuid(korpis_core::id::IdKind::Run, run.unwrap().0);
    let cancel = pb::CancelRunRequest {
        mutation: mutation("cancel").into(),
        run: run.to_string(),
        ..Default::default()
    };
    cp.client(pb::RunServiceClient::new).cancel_run(cancel).await.unwrap();
    for _ in 0..100 {
        if objects_of(&runtime, &w).is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the object of a run named no more was kept: {:?}", runtime.objects());
}

#[tokio::test]
async fn a_run_is_stopped_by_its_node_at_its_timeout() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let runtime = InMemory::new();
    let _agent = node(&cp, &dir, &runtime).await;
    let policy = pb::RunPolicy {
        timeout: korpis_wire::duration(Duration::from_secs(1)).into(),
        ..Default::default()
    };
    let w = task(&cp, &project, "slow", policy).await;
    counted(&cp, STARTED, 1).await;
    counted(&cp, ENDED, 1).await;
    let ended: Option<(String,)> = cp
        .query_one("SELECT after::text FROM effect WHERE action = 'run.ended' AND $1 <> ''", "x")
        .await;
    assert!(ended.unwrap().0.contains("timeout"));
    assert!(objects_of(&runtime, &w).iter().all(|(_, running)| !running), "stopped");
}

#[tokio::test]
async fn an_agent_restarted_mid_run_takes_the_run_over_and_never_starts_it_again() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let runtime = InMemory::new();
    let state = joined(&cp, &dir).await;
    let agent = running(&state, &runtime).await;
    let w = task(&cp, &project, "long", pb::RunPolicy::default()).await;
    counted(&cp, STARTED, 1).await;
    let before = objects_of(&runtime, &w);
    drop(agent);

    let _again = running(&state, &runtime).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(cp.scalar::<i64>(STARTED).await, 1, "taken over, not started again");
    assert_eq!(objects_of(&runtime, &w), before, "untouched, still running");
    let [(object, true)] = before.as_slice() else { panic!("{before:?}") };
    runtime.exit(object, 7);
    counted(&cp, ENDED, 1).await;
}
