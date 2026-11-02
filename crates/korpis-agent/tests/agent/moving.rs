// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload moved between two real agents, as far as `verify`: the destination, a standby told
//! no plan, prepares the target and reports it; nothing crosses for a workload holding no local
//! volume; the source stops the workload and reports `quiesce` once it has. Abandoned there, the
//! source runs it again and the destination is told nothing of it.

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
use std::time::Duration;

/// Node `name` joined and its agent started over `runtime`; the agent.
async fn node(cp: &ControlPlane, dir: &tempfile::TempDir, name: &str, runtime: &InMemory) -> Agent {
    let (token, ca) = cp.token(name).await;
    let state = StateDir::new(dir.path().join(name));
    let request =
        Join { server: cp.url.clone(), token, ca_fingerprint: ca, hostname: String::new() };
    join(&state, &request).await.unwrap();
    let options = Options { period: Duration::from_millis(200), ..Options::default() };
    let host = InMemoryHost::default();
    let agent = Agent::start(&state, runtime.clone(), host, Arc::new(facts), options).unwrap();
    until(&mut agent.events(), |e| matches!(e, Event::Welcomed { .. })).await;
    agent
}

/// Whether `workload` is observed running.
async fn running(cp: &ControlPlane, workload: &str) -> bool {
    let s = cp.status(workload).await;
    let state = s.observation.as_option().and_then(|o| o.state.as_known());
    state == Some(pb::ObservedState::OBSERVED_STATE_RUNNING)
}

/// The newest migration of `workload`.
async fn migration(cp: &ControlPlane, workload: &str) -> Option<pb::Migration> {
    let listed = pb::ListMigrationsRequest { workload: workload.to_owned(), ..Default::default() };
    let all = cp.workloads().list_migrations(listed).await.unwrap().into_owned().migrations;
    all.into_iter().last()
}

/// Moves `workload` to the node `to`, approved; the migration.
async fn moved(cp: &ControlPlane, workload: &str, to: &str) -> String {
    let listed = pb::ListNodesRequest { organization: cp.root_org.clone(), ..Default::default() };
    let nodes = cp.nodes().list_nodes(listed).await.unwrap().into_owned().nodes;
    let node = nodes.into_iter().find(|n| n.name == to).unwrap().id;
    let request = pb::MigrateWorkloadRequest {
        mutation: mutation("move").into(),
        workload: workload.to_owned(),
        node,
        ..Default::default()
    };
    let proposed = cp.workloads().migrate_workload(request).await.unwrap().into_owned();
    let plan = proposed.plan.into_option().unwrap().id;
    let approve =
        pb::ApprovePlanRequest { mutation: mutation("go").into(), plan, ..Default::default() };
    cp.client(pb::PlanServiceClient::new).approve_plan(approve).await.unwrap();
    migration(cp, workload).await.unwrap().id
}

#[tokio::test]
async fn a_moving_workload_is_prepared_there_quiesced_here_and_runs_here_again_once_abandoned() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = (InMemory::default(), InMemory::default());
    let _source = node(&cp, &dir, "fra-1", &here).await;
    let project = cp.project().await;
    let w = cp.declare(&project, "db").await;
    cp.announced(|| async { running(&cp, &w).await.then_some(()) }).await;
    let _destination = node(&cp, &dir, "fra-2", &there).await;

    let id = moved(&cp, &w, "fra-2").await;
    let verify = Some(pb::MigrationPhase::MIGRATION_PHASE_VERIFY);
    cp.announced(|| async {
        migration(&cp, &w).await.filter(|m| m.phase.as_known() == verify).map(drop)
    })
    .await;
    assert!(there.calls().contains(&format!("prepare {w}")), "prepared there");
    let made_there = there.objects().into_values().any(|(of, _)| of == w);
    assert!(!made_there, "a standby makes nothing");
    let runs_here = here.objects().into_values().any(|(of, running)| of == w && running);
    assert!(!runs_here, "stopped here before it was reported quiesced");

    let abandon = pb::AbandonMigrationRequest {
        mutation: mutation("abandon").into(),
        migration: id,
        reason: "not now".into(),
        ..Default::default()
    };
    cp.workloads().abandon_migration(abandon).await.unwrap();
    cp.announced(|| async { running(&cp, &w).await.then_some(()) }).await;
    let runs_here = here.objects().into_values().any(|(of, running)| of == w && running);
    assert!(runs_here, "the source runs it again");
}
