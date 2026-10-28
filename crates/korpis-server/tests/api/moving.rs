// SPDX-License-Identifier: AGPL-3.0-or-later

//! Moving a workload to another node (§8.1 of docs/05-scheduling.md), as far as its proposal,
//! its approval, and its abandonment. It is proposed as a plan of kind `migrate` naming the node
//! the scheduler chose among the others, which a person approves whatever the policy would
//! allow; approved, the migration is in `prepare`, the destination a placement that is not
//! authoritative, holding its capacity on that node; abandoned, the destination is freed and the
//! plan cancelled. A workload on no node, a move to the node it is on, a second migration, and a
//! volume whose class cannot cross between the two nodes are refused, and so is the approval of a
//! move onto a node the scheduler chose that was cordoned since.

use crate::agent::{Stream, TestAgent, welcome};
use crate::edge_groups::cordon;
use crate::journal::{ack, assignment, complete, send, until};
use crate::lifecycle::approve;
use crate::recipes::{body, create, project, push_paper};
use crate::streams::held;
use crate::support::{Harness, mutation, reason};
use crate::volumes::{ensured, volume_record};
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

/// A node's one class, on its own disk, saying whether its helper syncs trees.
fn local(tree_sync: bool) -> Vec<pb::StorageClassInfo> {
    vec![pb::StorageClassInfo {
        name: "local".into(),
        backend: pb::StorageBackend::STORAGE_BACKEND_DIRECTORY.into(),
        capabilities: pb::StorageCapabilities {
            fencing: pb::Fencing::FENCING_NONE.into(),
            tree_sync,
            ..Default::default()
        }
        .into(),
        capacity_bytes: 1 << 40,
        free_bytes: 1 << 39,
        ..Default::default()
    }]
}

fn moving(key: &str, workload: &str, node: &str) -> pb::MigrateWorkloadRequest {
    pb::MigrateWorkloadRequest {
        mutation: mutation(key).into(),
        workload: workload.to_owned(),
        node: node.to_owned(),
        ..Default::default()
    }
}

/// `db`, made on the node whose stream is `s`, its volume reported there.
async fn made(h: &Harness, s: &mut Stream) -> String {
    let repo = push_paper().await;
    let project = project(h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(create("db", &project, "db", body(&repo, &[])));
    let db = declared.await.unwrap().into_owned().workload.into_option().unwrap().id;
    let told = until(s, assignment(|t| t.workload == db && t.plan.is_set())).await;
    let last = complete(s, &db, told.plan.as_option().unwrap(), 1).await;
    held(s, &db).await;
    send(s, vec![volume_record(last + 1, ensured(&db, "data", true, false))]).await;
    assert_eq!(until(s, ack).await, last + 1);
    db
}

/// The placements of `workload`: each node, and whether it is authoritative.
async fn placements(h: &Harness, workload: &str) -> Vec<(String, bool)> {
    let store = h.db.store().await.unwrap();
    let q = "SELECT n.name, p.authoritative FROM placement p JOIN node n ON n.id = p.node_id
              WHERE p.workload_id = $1 ORDER BY n.name";
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload).unwrap();
    sqlx::query_as(q).bind(id.uuid()).fetch_all(store.pool()).await.unwrap()
}

#[tokio::test]
async fn a_move_is_a_plan_a_person_approves_and_abandoning_it_frees_the_destination() {
    let h = Harness::new().await;
    let (a, b) = (TestAgent::enroll(&h, "fra-1", 1).await, TestAgent::enroll(&h, "fra-2", 2).await);
    let (mut sa, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], local(true))).await;
    welcome(&first.unwrap());
    let db = made(&h, &mut sa).await;
    let (_sb, first) =
        TestAgent::open(&h, b.hello_with(SystemTime::now(), &[2; 16], local(true))).await;
    welcome(&first.unwrap());
    let workloads = h.workloads(h.root());

    let proposed = workloads.migrate_workload(moving("m1", &db, "")).await.unwrap().into_owned();
    let plan = proposed.plan.into_option().unwrap();
    assert_eq!(plan.kind.as_known(), Some(pb::PlanKind::PLAN_KIND_MIGRATE));
    assert_eq!(plan.status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_PROPOSED), "a person");
    let [step] = plan.steps.as_slice() else { panic!("{:?}", plan.steps) };
    assert_eq!(step.action, "migrate");
    assert!(step.disruptive, "it stops the workload");

    h.plans(h.root()).approve_plan(approve("move", &plan.id)).await.unwrap();
    let list = pb::ListMigrationsRequest { workload: db.clone(), ..Default::default() };
    let got = workloads.list_migrations(list.clone()).await.unwrap().into_owned();
    let [shown] = got.migrations.as_slice() else { panic!("{:?}", got.migrations) };
    assert_eq!(shown.phase.as_known(), Some(pb::MigrationPhase::MIGRATION_PHASE_PREPARE));
    assert_eq!(
        (shown.source.as_str(), shown.destination.as_str()),
        (a.node.as_str(), b.node.as_str())
    );
    let [volume] = shown.volumes.as_slice() else { panic!("{:?}", shown.volumes) };
    let tree = Some(pb::MigrationCrossing::MIGRATION_CROSSING_TREE);
    assert_eq!((volume.name.as_str(), volume.crossing.as_known()), ("data", tree));
    let both = vec![("fra-1".to_owned(), true), ("fra-2".to_owned(), false)];
    assert_eq!(placements(&h, &db).await, both, "the destination holds its capacity");

    let again = workloads.migrate_workload(moving("m2", &db, "")).await.unwrap_err();
    assert_eq!(reason(&again), "workload.migrating");
    let abandon = pb::AbandonMigrationRequest {
        mutation: mutation("abandon").into(),
        migration: shown.id.clone(),
        reason: "the destination's disk is needed elsewhere".into(),
        ..Default::default()
    };
    let ended = workloads.abandon_migration(abandon).await.unwrap().into_owned();
    let ended = ended.migration.into_option().unwrap();
    let abandoned = Some(pb::MigrationOutcome::MIGRATION_OUTCOME_ABANDONED);
    assert_eq!(ended.outcome.as_known(), abandoned);
    assert_eq!(placements(&h, &db).await, vec![("fra-1".to_owned(), true)], "freed");
    let get = pb::GetPlanRequest { id: plan.id.clone(), ..Default::default() };
    let p = h.plans(h.root()).get_plan(get).await.unwrap().into_owned().plan.into_option();
    assert_eq!(p.unwrap().status.as_known(), Some(pb::PlanStatus::PLAN_STATUS_CANCELLED));
}

#[tokio::test]
async fn a_move_is_refused_off_no_node_onto_its_own_and_where_a_volume_cannot_cross() {
    let h = Harness::new().await;
    let (a, b) = (TestAgent::enroll(&h, "fra-1", 1).await, TestAgent::enroll(&h, "fra-2", 2).await);
    let (mut sa, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], local(true))).await;
    welcome(&first.unwrap());
    let db = made(&h, &mut sa).await;
    let (_sb, first) =
        TestAgent::open(&h, b.hello_with(SystemTime::now(), &[2; 16], local(false))).await;
    welcome(&first.unwrap());
    let workloads = h.workloads(h.root());

    let own = workloads.migrate_workload(moving("m1", &db, &a.node)).await.unwrap_err();
    assert_eq!(reason(&own), "request.invalid", "the node it is on");
    let refused = workloads.migrate_workload(moving("m2", &db, &b.node)).await.unwrap_err();
    assert_eq!(reason(&refused), "volume.uncrossable");
    assert!(refused.message.as_deref().unwrap_or_default().contains("destination"), "{refused:?}");

    for n in [&a.node, &b.node] {
        cordon(&h, n).await;
    }
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "spare").await;
    let declared = workloads.declare_intent(create("idle", &project, "idle", body(&repo, &[])));
    let idle = declared.await.unwrap().into_owned().workload.into_option().unwrap().id;
    let unplaced = workloads.migrate_workload(moving("m3", &idle, "")).await.unwrap_err();
    assert_eq!(reason(&unplaced), "workload.unplaced");
}

#[tokio::test]
async fn a_destination_the_scheduler_chose_is_refused_at_approval_once_cordoned() {
    let h = Harness::new().await;
    let (a, b) = (TestAgent::enroll(&h, "fra-1", 1).await, TestAgent::enroll(&h, "fra-2", 2).await);
    let (mut sa, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], local(true))).await;
    welcome(&first.unwrap());
    let db = made(&h, &mut sa).await;
    let (_sb, first) =
        TestAgent::open(&h, b.hello_with(SystemTime::now(), &[2; 16], local(true))).await;
    welcome(&first.unwrap());
    let workloads = h.workloads(h.root());
    let proposed = workloads.migrate_workload(moving("m1", &db, "")).await.unwrap().into_owned();
    let plan = proposed.plan.into_option().unwrap();
    let by = plan.steps[0].detail.get("chosen_by").and_then(|v| v.kind.as_ref());
    let scheduler = pb::__buffa::oneof::value::Kind::StringValue("scheduler".into());
    assert_eq!(by, Some(&scheduler));

    // A cordon is overridden only for a node an operator named, never for the scheduler's.
    cordon(&h, &b.node).await;
    let refused = h.plans(h.root()).approve_plan(approve("move", &plan.id)).await.unwrap_err();
    assert_eq!(reason(&refused), "placement.unsatisfiable");
    assert_eq!(placements(&h, &db).await, vec![("fra-1".to_owned(), true)], "nothing held");
}
