// SPDX-License-Identifier: AGPL-3.0-or-later

//! The reconciler against a real control plane, with the in-memory runtime standing in for a
//! driver (§4 of docs/02-architecture.md): a declared workload is placed, its plan executed step
//! by step under a lease, and it converges; the agent restarts and adopts it without touching
//! it; a crash is restarted; and a retirement removes the runtime object, keeps the volumes,
//! releases the lease, and retires the workload.

use crate::session::until;
use crate::support::{ControlPlane, facts};
use korpis_agent::host::InMemoryHost;
use korpis_agent::join::{Join, join};
use korpis_agent::journal::{Bounds, JournalHandle};
use korpis_agent::lease::{Keeper, Leases};
use korpis_agent::reconcile::progress::Progress;
use korpis_agent::reconcile::{Reconciler, Wiring};
use korpis_agent::runtime::memory::InMemory;
use korpis_agent::runtime::{Prepared, Runtime};
use korpis_agent::session::{Event, Session};
use korpis_agent::state::{Identity, StateDir};
use korpis_proto::korpis::v1 as pb;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

/// An agent: its session, lease keeper, journal maintenance, and reconciler.
struct Agent {
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Agent {
    fn drop(&mut self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

async fn start(
    identity: &Identity,
    state: &StateDir,
    runtime: &InMemory,
    host: &InMemoryHost,
) -> Agent {
    let journal = JournalHandle::open(&state.root().join("journal"), Bounds::default()).unwrap();
    let (session, link) =
        Session::new(identity.clone(), state.clone(), Arc::new(facts), journal.clone());
    let mut events = link.events.subscribe();
    let leases = Leases::new(journal.clone());
    let keeper =
        Keeper::new(leases.clone(), link.held.clone(), link.inbound, link.outbound.clone());
    let wiring = Wiring {
        node: identity.node.clone(),
        held: link.held.clone(),
        leases,
        journal: journal.clone(),
        outbound: link.outbound.clone(),
        events: link.events.subscribe(),
        period: Duration::from_millis(200),
    };
    let progress = Progress::open(&state.root().join("progress")).unwrap();
    let reconciler = Reconciler::new(runtime.clone(), host.clone(), wiring, progress);
    let tasks = vec![
        tokio::spawn(session.run()),
        tokio::spawn(keeper.run()),
        journal.maintain_every(Duration::from_millis(500)),
        tokio::spawn(reconciler.run()),
    ];
    until(&mut events, |e| matches!(e, Event::Welcomed { .. })).await;
    Agent { tasks }
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
) -> (Identity, StateDir, String) {
    let (token, ca) = cp.token("fra-7").await;
    let state = StateDir::new(dir.path().join("agent"));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    let identity = join(&state, &request).await.unwrap();
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
    (identity, state, orphan)
}

/// The workload converged, running in its one runtime object, every completion granted; the
/// object's identifier.
async fn converges(cp: &ControlPlane, w: &str, runtime: &InMemory, orphan: &str) -> String {
    let s = status(cp, w, converged).await;
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
    assert!(effects(cp, "SELECT count(*) FROM effect WHERE action = 'node.restart'").await >= 1);
}

#[tokio::test]
async fn a_workload_converges_survives_an_agent_restart_untouched_and_retires() {
    let cp = ControlPlane::start().await;
    let project = cp.project().await;
    let dir = tempfile::tempdir().unwrap();
    let (runtime, host) = (InMemory::new(), InMemoryHost::new());
    let (identity, state, orphan) = node(&cp, &dir, &runtime).await;
    let agent = start(&identity, &state, &runtime, &host).await;
    let w = cp.declare(&project, "mc").await;
    let id = converges(&cp, &w, &runtime, &orphan).await;
    assert_eq!(host.content().len(), 1, "the recipe's content was fetched once");

    drop(agent);
    let before = runtime.calls().len();
    let agent = start(&identity, &state, &runtime, &host).await;
    adopted(&cp, &w, &runtime, before).await;
    revived(&cp, &w, &runtime, &id).await;

    // Retirement: the object goes, the workload is retired, the orphan stays.
    cp.retire(&w).await;
    counted(&cp, "SELECT count(*) FROM effect WHERE action = 'workload.retire'", 1).await;
    assert_eq!(runtime.objects().len(), 1, "the runtime object is destroyed, the orphan kept");
    drop(agent);
}
