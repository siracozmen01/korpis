// SPDX-License-Identifier: AGPL-3.0-or-later

//! A workload moved between two real agents. The destination, a standby told no plan, prepares
//! the target and reports it; nothing crosses for a workload holding no local volume; the source
//! stops the workload and reports `quiesce` once it has; both report `verify`, the destination
//! having prepared again; the source releases its lease, and the destination, granted the next,
//! makes and starts the workload and reports it running; once the hold ends the source deletes
//! what it kept. Abandoned while the source stops, the workload runs where it was again.

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

/// Whether `runtime` runs an object of `workload`, and whether it has one at all.
fn object_of(runtime: &InMemory, workload: &str) -> (bool, bool) {
    let objects: Vec<bool> =
        runtime.objects().into_values().filter(|(of, _)| of == workload).map(|o| o.1).collect();
    (objects.iter().any(|r| *r), !objects.is_empty())
}

/// The newest migration of `workload`, once `pick` holds of it.
async fn migration_once(
    cp: &ControlPlane,
    workload: &str,
    pick: impl Fn(&pb::Migration) -> bool,
) -> pb::Migration {
    cp.announced(|| async { migration(cp, workload).await.filter(|m| pick(m)) }).await
}

#[tokio::test]
async fn a_workload_moves_to_another_node_and_its_source_lets_go_once_the_hold_ends() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = (InMemory::default(), InMemory::default());
    let _source = node(&cp, &dir, "fra-1", &here).await;
    let project = cp.project().await;
    let w = cp.declare(&project, "db").await;
    cp.announced(|| async { running(&cp, &w).await.then_some(()) }).await;
    let _destination = node(&cp, &dir, "fra-2", &there).await;

    let id = moved(&cp, &w, "fra-2").await;
    let release = Some(pb::MigrationPhase::MIGRATION_PHASE_RELEASE);
    migration_once(&cp, &w, |m| m.phase.as_known() == release).await;
    assert_eq!(object_of(&there, &w), (true, true), "it runs there");
    assert_eq!(object_of(&here, &w), (false, true), "stopped here, kept through the hold");

    let early = pb::ReleaseMigrationRequest {
        mutation: mutation("release").into(),
        migration: id,
        ..Default::default()
    };
    cp.workloads().release_migration(early).await.unwrap();
    let completed = Some(pb::MigrationOutcome::MIGRATION_OUTCOME_COMPLETED);
    migration_once(&cp, &w, |m| m.outcome.as_known() == completed).await;
    assert_eq!(object_of(&here, &w), (false, false), "nothing of it is left here");
    assert!(running(&cp, &w).await, "observed running there");
}

#[tokio::test]
async fn a_migration_abandoned_while_its_source_stops_leaves_it_running_where_it_was() {
    let cp = ControlPlane::start().await;
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = (InMemory::default(), InMemory::default());
    let _source = node(&cp, &dir, "fra-1", &here).await;
    let project = cp.project().await;
    let w = cp.declare(&project, "db").await;
    cp.announced(|| async { running(&cp, &w).await.then_some(()) }).await;
    let _destination = node(&cp, &dir, "fra-2", &there).await;
    here.hold("stop", true);

    let id = moved(&cp, &w, "fra-2").await;
    let quiesce = Some(pb::MigrationPhase::MIGRATION_PHASE_QUIESCE);
    migration_once(&cp, &w, |m| m.phase.as_known() == quiesce).await;
    assert!(there.calls().contains(&format!("prepare {w}")), "prepared there");
    assert_eq!(object_of(&there, &w), (false, false), "a standby makes nothing");

    let abandon = pb::AbandonMigrationRequest {
        mutation: mutation("abandon").into(),
        migration: id,
        reason: "not now".into(),
        ..Default::default()
    };
    cp.workloads().abandon_migration(abandon).await.unwrap();
    here.hold("stop", false);
    let abandoned = Some(pb::MigrationOutcome::MIGRATION_OUTCOME_ABANDONED);
    migration_once(&cp, &w, |m| m.outcome.as_known() == abandoned).await;
    cp.announced(|| async { (object_of(&here, &w).0 && running(&cp, &w).await).then_some(()) })
        .await;
    assert_eq!(object_of(&there, &w), (false, false));
}
