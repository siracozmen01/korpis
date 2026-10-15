// SPDX-License-Identifier: AGPL-3.0-or-later

//! The reconciler against a real control plane, with the in-memory runtime standing in for a
//! driver (§4 of docs/02-architecture.md): a declared workload is placed, its plan executed step
//! by step under a lease, and it converges, all wired as `korpis-agent run` wires them; the agent restarts and adopts it without touching
//! it; a crash is restarted as soon as the runtime reports it; and a retirement removes the runtime object, keeps the volumes,
//! which the node reported as it made them, releases the lease, and retires the workload. The
//! node reports the recipe whose content it pulled, for content locality.

use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::run::{Agent, Options};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::runtime::{Prepared, Runtime};
use korpis_agent::session::Event;
use korpis_agent::state::StateDir;
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::time::Duration;

async fn start(
    state: &StateDir,
    runtime: &InMemory,
    host: &InMemoryHost,
    period: Duration,
) -> Agent {
    let options =
        Options { period, maintain_every: Duration::from_millis(500), ..Options::default() };
    let agent =
        Agent::start(state, runtime.clone(), host.clone(), Arc::new(facts), options).unwrap();
    let mut events = agent.events();
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    agent
}

/// Waits until `pick` holds of the workload's status.
async fn status(
    cp: &ControlPlane,
    w: &str,
    pick: impl Fn(&pb::WorkloadStatus) -> bool,
) -> pb::WorkloadStatus {
    for _ in 0..200 {
        let s = cp.status(w).await;
        if pick(&s) {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{w} never reached the status awaited: {:?}", cp.status(w).await);
}

fn converged(s: &pb::WorkloadStatus) -> bool {
    s.convergence.as_known() == Some(pb::Convergence::CONVERGENCE_CONVERGED)
}

async fn effects(cp: &ControlPlane, sql: &'static str) -> i64 {
    cp.scalar(sql).await
}

/// Waits until `sql` counts `n`.
async fn counted(cp: &ControlPlane, sql: &'static str, n: i64) {
    for _ in 0..100 {
        if effects(cp, sql).await == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(effects(cp, sql).await, n, "{sql}");
}

/// A node joined to `cp`, with an object on it no assignment claims; that object's identifier.
async fn node(
    cp: &ControlPlane,
    dir: &tempfile::TempDir,
    runtime: &InMemory,
) -> (StateDir, String) {
    let (token, ca) = cp.token("fra-7").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let stranger = pb::Intent { id: "in_stranger".into(), ..Default::default() };
    let Ok(Prepared::Spec(mut spec)) = runtime.prepare("wl_stranger", &stranger, b"").await else {
        panic!("the memory runtime prepares anything");
    };
    spec.labels = korpis_proto::korpis::driver::v1::RuntimeLabels {
        workload: "wl_stranger".into(),
        ..Default::default()
    }
    .into();
    let orphan = runtime.create(&spec).await.unwrap();
    (state, orphan)
}

/// The workload converged, running in its one runtime object, every completion granted; the
/// object's identifier.
async fn converges(cp: &ControlPlane, w: &str, runtime: &InMemory, orphan: &str) -> String {
    let s = Box::pin(status(cp, w, converged)).await;
    let observed = s.observation.as_option().unwrap();
    assert_eq!(observed.state.as_known(), Some(pb::ObservedState::OBSERVED_STATE_RUNNING));
    let mut objects = runtime.objects();
    assert!(objects.remove(orphan).is_some(), "the orphan is untouched");
    assert_eq!(effects(cp, "SELECT count(*) FROM effect WHERE action = 'node.orphan'").await, 1);
    assert_eq!(objects.len(), 1);
    let (id, (workload, running)) = objects.into_iter().next().unwrap();
    assert!(running && workload == w);
    assert_eq!(observed.runtime_id, id);
    let ungranted = effects(
        cp,
        "SELECT count(*) FROM effect WHERE action LIKE 'step.%' AND origin_seq IS NOT NULL \
         AND grant_id IS NULL",
    )
    .await;
    assert_eq!(ungranted, 0, "every step completion names the grant its plan was approved under");
    id
}

/// After a restart the workload is adopted, never stopped or started again.
async fn adopted(cp: &ControlPlane, w: &str, runtime: &InMemory, before: usize) {
    counted(cp, "SELECT count(*) FROM effect WHERE action = 'node.adopt'", 1).await;
    status(cp, w, converged).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let after: Vec<String> = runtime.calls().split_off(before);
    assert!(
        after.iter().all(|c| c == "recover "),
        "the restarted agent only enumerated what it adopted: {after:?}"
    );
    let sql = "SELECT count(*) FROM effect WHERE action = 'node.adopt' AND grant_id IS NOT NULL";
    assert_eq!(effects(cp, sql).await, 1, "keeping an approved state names its grant");
}

/// A crash is brought back, and recorded.
async fn revived(cp: &ControlPlane, w: &str, runtime: &InMemory, id: &str) {
    runtime.exit(id, 3);
    let up = || runtime.objects().get(id).is_some_and(|(_, running)| *running);
    for _ in 0..100 {
        if up() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(up(), "restarted");
    status(cp, w, converged).await;
    // The restart is journalled and reaches the control plane after the observation that says
    // the workload runs again, so it is waited for.
    let restarts = "SELECT count(*) FROM effect WHERE action = 'node.restart'";
    for _ in 0..100 {
        if effects(cp, restarts).await >= 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the restart was never recorded");
}

#[tokio::test]
async fn a_workload_converges_survives_an_agent_restart_untouched_and_retires() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let (state, orphan) = node(&cp, &dir, &runtime).await;
    let agent = start(&state, &runtime, &host, Duration::from_millis(200)).await;
    let w = cp.declare(&project, "mc").await;
    let id = converges(&cp, &w, &runtime, &orphan).await;
    // The image the recipe runs; for its install, the recipe with its files (a template reads
    // one) and the jar its `fetch` names, by the hash it declares (§4 of docs/09-recipes.md).
    let content = host.content();
    assert_eq!(content.len(), 3, "each fetched once: {content:?}");
    assert!(content.iter().any(|c| c == &format!("sha256:{}", "1".repeat(64))), "{content:?}");
    let installed =
        "SELECT count(*) FROM effect WHERE action = 'step.install' AND outcome = 'succeeded'";
    assert_eq!(effects(&cp, installed).await, 1, "installed once, before it was created");
    let ids = host.ids();
    let (base, count) = ids.get(&w).copied().expect("its id range was recorded before create");
    assert!(base >= 1 << 30 && count == 1 << 16, "a workload slot: {base} {count}");
    let attached = "SELECT count(*) FROM volume WHERE name = 'data' AND attached_to IS NOT NULL";
    counted(&cp, attached, 1).await;
    // Its recipe's content is held here now, and the node said so (§2.2 of 05-scheduling.md).
    counted(&cp, "SELECT count(*) FROM node_content", 1).await;

    agent.stop().await;
    let before = runtime.calls().len();
    // A period nothing in the test outlasts: from here on the reconciler moves only when it
    // is told something changed, so the crash below is seen because the runtime reported it.
    let agent = start(&state, &runtime, &host, Duration::from_secs(3600)).await;
    adopted(&cp, &w, &runtime, before).await;
    revived(&cp, &w, &runtime, &id).await;

    // Retirement: the object goes, the workload is retired, the orphan stays.
    cp.retire(&w).await;
    counted(&cp, "SELECT count(*) FROM effect WHERE action = 'workload.retire'", 1).await;
    assert_eq!(runtime.objects().len(), 1, "the runtime object is destroyed, the orphan kept");
    let kept = "SELECT count(*) FROM volume WHERE name = 'data' AND attached_to IS NULL \
                AND detached_at IS NOT NULL AND deleted_at IS NULL";
    assert_eq!(effects(&cp, kept).await, 1, "its volume is detached and kept");
    drop(agent);
}

/// Waits until the runtime has been asked for `call`.
async fn called(runtime: &InMemory, call: &str) {
    for _ in 0..200 {
        if runtime.calls().iter().any(|c| c == call) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{call} never happened: {:?}", runtime.calls());
}

#[tokio::test]
async fn a_cancelled_plan_finishes_the_step_in_flight_and_begins_no_other() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let (state, _) = node(&cp, &dir, &runtime).await;
    runtime.hold("create", true);
    let agent = start(&state, &runtime, &host, Duration::from_millis(200)).await;
    let w = cp.declare(&project, "mc").await;
    called(&runtime, "held create").await;
    let plan = cp.status(&w).await.active_plan.into_option().unwrap();
    cp.cancel(&plan.id).await;
    // The cancellation reaches the node while the step is in flight; then the step finishes.
    tokio::time::sleep(Duration::from_millis(600)).await;
    runtime.hold("create", false);
    counted(&cp, "SELECT count(*) FROM effect WHERE action = 'step.create'", 1).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let started = effects(&cp, "SELECT count(*) FROM effect WHERE action = 'step.start'").await;
    assert_eq!(started, 0, "the step after the one in flight is never begun");
    let create = plan.steps.iter().find(|s| s.action == "create").unwrap().index;
    let now = cp.plan(&plan.id).await;
    assert_eq!(now.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_CANCELLED));
    assert_eq!(now.completed, Some(create), "the step in flight counts");
    drop(agent);
}
