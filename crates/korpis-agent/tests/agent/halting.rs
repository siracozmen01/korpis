// SPDX-License-Identifier: AGPL-3.0-or-later

//! A stop that takes its whole grace holds up nothing else on the node (§6 of
//! docs/02-architecture.md): an object that ignores the terminating signal, as a container's
//! first process with no handler does, is killed once its grace is over, while the reconciler
//! goes on starting the node's other workloads; and whatever needed it stopped, a retirement or a
//! run's cancellation, completes once it has.

use crate::session::until;
use crate::support::{ControlPlane, facts, mutation};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A node joined to `cp` and running, giving each object `grace` to stop once signalled.
async fn node(
    cp: &ControlPlane,
    dir: &tempfile::TempDir,
    runtime: &InMemory,
    grace: Duration,
) -> Agent {
    let (token, ca) = cp.token("fra-8").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let options = Options {
        period: Duration::from_millis(200),
        stop_grace: grace,
        maintain_every: Duration::from_millis(500),
        ..Options::default()
    };
    let host = InMemoryHost::default();
    let agent = Agent::start(&state, runtime.clone(), host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    agent
}

/// The running object of `workload`, once there is one.
async fn object(runtime: &InMemory, workload: &str) -> String {
    for _ in 0..300 {
        let found =
            runtime.objects().into_iter().find(|(_, (w, running))| w == workload && *running);
        if let Some((id, _)) = found {
            return id;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{workload} never ran: {:?}", runtime.objects());
}

/// Waits until the runtime has been asked for `call`.
async fn called(runtime: &InMemory, call: &str) {
    for _ in 0..600 {
        if runtime.calls().iter().any(|c| c == call) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{call} never happened: {:?}", runtime.calls());
}

/// Waits until `sql` counts `n`.
async fn counted(cp: &ControlPlane, sql: &'static str, n: i64) {
    for _ in 0..300 {
        if cp.scalar::<i64>(sql).await == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(cp.scalar::<i64>(sql).await, n, "{sql}");
}

#[tokio::test]
async fn an_object_that_ignores_its_stop_is_killed_past_its_grace_and_holds_up_no_other() {
    let grace = Duration::from_secs(20);
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let runtime = InMemory::new();
    let _agent = node(&cp, &dir, &runtime, grace).await;
    let deaf = cp.declare(&project, "deaf").await;
    let first = object(&runtime, &deaf).await;

    runtime.ignore_term(true);
    let retiring = Instant::now();
    cp.retire(&deaf).await;
    called(&runtime, &format!("signal {first}")).await;
    // While it has its grace, another workload is placed, built, and started.
    let other = cp.declare(&project, "other").await;
    let second = object(&runtime, &other).await;
    let killed = format!("kill {first}");
    let calls = runtime.calls();
    assert!(!calls.contains(&killed), "{second} started before {first} was killed: {calls:?}");

    // Then it is killed, past its grace, and its retirement completes.
    called(&runtime, &killed).await;
    assert!(retiring.elapsed() >= grace, "not before its grace was over");
    counted(&cp, "SELECT count(*) FROM effect WHERE action = 'workload.retire'", 1).await;
    assert!(!runtime.objects().contains_key(&first), "its object is destroyed");
}

#[tokio::test]
async fn a_cancelled_run_that_ignores_its_stop_is_killed_past_its_grace_and_ends_cancelled() {
    let grace = Duration::from_secs(3);
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let runtime = InMemory::new();
    let _agent = node(&cp, &dir, &runtime, grace).await;
    let w = crate::runs::task(&cp, &project, "build", pb::RunPolicy::default()).await;
    let running = object(&runtime, &w).await;

    runtime.ignore_term(true);
    let run: Option<(uuid::Uuid,)> =
        cp.query_one("SELECT id FROM run WHERE workload_id::text <> $1", "").await;
    let run = korpis_core::id::Id::from_uuid(korpis_core::id::IdKind::Run, run.unwrap().0);
    let cancel = pb::CancelRunRequest {
        mutation: mutation("cancel").into(),
        run: run.to_string(),
        ..Default::default()
    };
    let cancelling = Instant::now();
    cp.client(pb::RunServiceClient::new).cancel_run(cancel).await.unwrap();
    called(&runtime, &format!("signal {running}")).await;
    called(&runtime, &format!("kill {running}")).await;
    assert!(cancelling.elapsed() >= grace, "not before its grace was over");
    // Its end is journaled once it has stopped, saying why, and its object goes.
    counted(&cp, "SELECT count(*) FROM effect WHERE action = 'run.ended'", 1).await;
    let ended: Option<(String,)> = cp
        .query_one("SELECT after::text FROM effect WHERE action = 'run.ended' AND $1 <> ''", "x")
        .await;
    assert!(ended.unwrap().0.contains("cancelled"), "the node says why it stopped it");
    for _ in 0..100 {
        if !runtime.objects().contains_key(&running) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the run's object was never removed: {:?}", runtime.objects());
}
