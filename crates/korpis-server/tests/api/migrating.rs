// SPDX-License-Identifier: AGPL-3.0-or-later

//! A migration walked through its phases on both nodes' reports (§8.1 of docs/05-scheduling.md):
//! each phase advances once every side acting in it reported it done; the source's release of its
//! lease moves the placement, the volume with it onto the copy the destination holds, and the
//! destination is granted the next epoch; the source's deletion after the hold completes it. A
//! rollback after the move returns the workload to the source and keeps what the destination
//! wrote as a divergent copy; manifests that differ at `verify` abandon it, naming where; and a
//! report from a node that is not that side, or of another phase, is not believed.

use crate::agent::{Stream, TestAgent, welcome};
use crate::journal::{ack, assignment, complete, plan_status, send, until};
use crate::lifecycle::approve;
use crate::recipes::{body, create, project, push_paper};
use crate::streams::{held, up};
use crate::support::{Harness, mutation};
use crate::volumes::{ensured, volume_record};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::journal_record::Body as B;
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

use agent::MigrationPhase as P;
use agent::MigrationSide as S;

/// Two nodes, a workload with a volume made and running on the first under `epoch`, and a
/// migration of it to the second approved: each node's stream and its next journal sequence.
struct Moving {
    h: Harness,
    db: String,
    plan: String,
    migration: String,
    source: (Stream, u64),
    destination: (Stream, u64),
    epoch: u64,
    /// The destination's first assignment of the workload, carrying its role.
    told: agent::Assignment,
}

/// Waits until `workload`'s lease is released.
async fn released(h: &Harness, workload: &str) {
    let store = h.db.store().await.unwrap();
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload).unwrap();
    for _ in 0..200 {
        let q = "SELECT released_at IS NOT NULL FROM lease WHERE workload_id = $1";
        let done: bool =
            sqlx::query_scalar(q).bind(id.uuid()).fetch_one(store.pool()).await.unwrap();
        if done {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the lease was not released");
}

fn local() -> Vec<pb::StorageClassInfo> {
    vec![pb::StorageClassInfo {
        name: "local".into(),
        backend: pb::StorageBackend::STORAGE_BACKEND_DIRECTORY.into(),
        capabilities: pb::StorageCapabilities {
            fencing: pb::Fencing::FENCING_NONE.into(),
            tree_sync: true,
            ..Default::default()
        }
        .into(),
        capacity_bytes: 1 << 40,
        free_bytes: 1 << 39,
        ..Default::default()
    }]
}

async fn moving() -> Moving {
    let h = Harness::new().await;
    let (a, b) = (TestAgent::enroll(&h, "fra-1", 1).await, TestAgent::enroll(&h, "fra-2", 2).await);
    let (mut sa, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], local())).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(create("db", &project, "db", body(&repo, &[])));
    let db = declared.await.unwrap().into_owned().workload.into_option().unwrap().id;
    let told = until(&mut sa, assignment(|t| t.workload == db && t.plan.is_set())).await;
    let last = complete(&mut sa, &db, told.plan.as_option().unwrap(), 1).await;
    let epoch = held(&mut sa, &db).await;
    send(&mut sa, vec![volume_record(last + 1, ensured(&db, "data", true, false))]).await;
    assert_eq!(until(&mut sa, ack).await, last + 1);
    let (mut sb, first) =
        TestAgent::open(&h, b.hello_with(SystemTime::now(), &[2; 16], local())).await;
    welcome(&first.unwrap());
    let request = pb::MigrateWorkloadRequest {
        mutation: mutation("move").into(),
        workload: db.clone(),
        ..Default::default()
    };
    let proposed = workloads.migrate_workload(request).await.unwrap().into_owned();
    let plan = proposed.plan.into_option().unwrap().id;
    h.plans(h.root()).approve_plan(approve("go", &plan)).await.unwrap();
    let migration = shown(&h, &db).await.id;
    // A node's journal is believed only as far as its assignments reach, so the destination
    // reports nothing before it is told its role.
    let told = until(&mut sb, assignment(|t| t.workload == db && t.migration.is_set())).await;
    let (source, destination) = ((sa, last + 2), (sb, 1));
    Moving { h, db, plan, migration, source, destination, epoch, told }
}

/// The newest migration of `workload`.
async fn shown(h: &Harness, workload: &str) -> pb::Migration {
    let list = pb::ListMigrationsRequest { workload: workload.to_owned(), ..Default::default() };
    let got = h.workloads(h.root()).list_migrations(list).await.unwrap().into_owned();
    got.migrations.into_iter().next().unwrap()
}

fn report(m: &str, (phase, side): (P, S), done: bool) -> agent::MigrationProgress {
    let outcome = if done {
        agent::MigrationOutcomeOfPhase::MIGRATION_OUTCOME_OF_PHASE_DONE
    } else {
        agent::MigrationOutcomeOfPhase::MIGRATION_OUTCOME_OF_PHASE_FAILED
    };
    agent::MigrationProgress {
        migration: m.to_owned(),
        phase: phase.into(),
        side: side.into(),
        outcome: outcome.into(),
        ..Default::default()
    }
}

fn volume(pass: &str, held: bool, manifest: &str) -> agent::MigrationVolumeProgress {
    agent::MigrationVolumeProgress {
        name: "data".into(),
        pass: pass.into(),
        sent_bytes: if held { 0 } else { 4096 },
        held,
        helper_id: if held { "mig.data".into() } else { String::new() },
        manifest: manifest.into(),
        chunks: if manifest.is_empty() { Vec::new() } else { chunks(manifest) },
        ..Default::default()
    }
}

/// A manifest in two runs, the second's digest `last`.
fn chunks(last: &str) -> Vec<agent::ManifestChunk> {
    let run = |first_path: &str, digest: &str| agent::ManifestChunk {
        first_path: first_path.into(),
        digest: digest.into(),
        ..Default::default()
    };
    vec![run("", "sha256:top"), run("world/", last)]
}

/// Journals `p` on the node whose stream and next sequence `(s, seq)` are.
async fn journal((s, seq): &mut (Stream, u64), p: agent::MigrationProgress) {
    let r = agent::JournalRecord {
        seq: *seq,
        body: Some(B::Migration(Box::new(p))),
        count: 1,
        ..Default::default()
    };
    send(s, vec![r]).await;
    assert_eq!(until(s, ack).await, *seq);
    *seq += 1;
}

fn phase(m: &pb::Migration) -> Option<pb::MigrationPhase> {
    m.phase.as_known()
}

/// Walks `m` through `verify`, both manifests `(source, destination)`.
async fn through_verify(m: &mut Moving, manifests: (&str, &str)) {
    let id = m.migration.clone();
    journal(
        &mut m.destination,
        report(&id, (P::MIGRATION_PHASE_PREPARE, S::MIGRATION_SIDE_DESTINATION), true),
    )
    .await;
    assert_eq!(
        phase(&shown(&m.h, &m.db).await),
        Some(pb::MigrationPhase::MIGRATION_PHASE_REPLICATE)
    );
    let pass = uuid::Uuid::now_v7().to_string();
    for (phase_of, quiesce) in
        [(P::MIGRATION_PHASE_REPLICATE, false), (P::MIGRATION_PHASE_FINAL_SYNC, true)]
    {
        if quiesce {
            let mut q = report(&id, (P::MIGRATION_PHASE_QUIESCE, S::MIGRATION_SIDE_SOURCE), true);
            q.watermarks = vec![agent::StreamWatermark {
                stream: "console".into(),
                next_offset: 10,
                journaled_to: 5,
                ..Default::default()
            }];
            journal(&mut m.source, q).await;
        }
        let mut sent = report(&id, (phase_of, S::MIGRATION_SIDE_SOURCE), true);
        sent.volumes = vec![volume(&pass, false, "")];
        journal(&mut m.source, sent).await;
        let mut got = report(&id, (phase_of, S::MIGRATION_SIDE_DESTINATION), true);
        got.volumes = vec![volume(&pass, true, "")];
        journal(&mut m.destination, got).await;
    }
    assert_eq!(phase(&shown(&m.h, &m.db).await), Some(pb::MigrationPhase::MIGRATION_PHASE_VERIFY));
    let mut a = report(&id, (P::MIGRATION_PHASE_VERIFY, S::MIGRATION_SIDE_SOURCE), true);
    a.volumes = vec![volume(&pass, false, manifests.0)];
    journal(&mut m.source, a).await;
    let mut b = report(&id, (P::MIGRATION_PHASE_VERIFY, S::MIGRATION_SIDE_DESTINATION), true);
    b.volumes = vec![volume(&pass, true, manifests.1)];
    journal(&mut m.destination, b).await;
}

/// Releases the source's lease, so the placement moves, and has the destination start the
/// workload under the next epoch and report it; that epoch.
async fn cut_over(m: &mut Moving) -> u64 {
    let released =
        agent::LeaseReleased { workload: m.db.clone(), epoch: m.epoch, ..Default::default() };
    up(&mut m.source.0, Up::LeaseReleased(Box::new(released))).await;
    let db = m.db.clone();
    let told = until(
        &mut m.destination.0,
        assignment(|t| {
            t.workload == db && t.migration.as_option().is_some_and(|r| !r.watermarks.is_empty())
        }),
    )
    .await;
    let [mark] = told.migration.as_option().unwrap().watermarks.as_slice() else { panic!() };
    assert_eq!((mark.stream.as_str(), mark.next_offset), ("console", 10), "handed over");
    let epoch = held(&mut m.destination.0, &m.db).await;
    assert!(epoch > m.epoch, "the next epoch");
    let id = m.migration.clone();
    journal(
        &mut m.destination,
        report(&id, (P::MIGRATION_PHASE_CUTOVER, S::MIGRATION_SIDE_DESTINATION), true),
    )
    .await;
    epoch
}

/// The node and helper identifier of each record of `workload`'s volume named `data`, and the
/// volume each diverged from, if one.
async fn records(h: &Harness, workload: &str) -> Vec<(String, String, bool)> {
    let store = h.db.store().await.unwrap();
    let q = "SELECT n.name, v.helper_id, v.divergent_of IS NOT NULL FROM volume v
               JOIN node n ON n.id = v.node_id
              WHERE v.name = 'data' AND v.deleted_at IS NULL
                AND v.project_id = (SELECT project_id FROM workload WHERE id = $1)
              ORDER BY n.name";
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload).unwrap();
    sqlx::query_as(q).bind(id.uuid()).fetch_all(store.pool()).await.unwrap()
}

fn on(node: &str, helper: &str, divergent: bool) -> (String, String, bool) {
    (node.to_owned(), helper.to_owned(), divergent)
}

#[tokio::test]
async fn a_migration_walks_every_phase_on_both_reports_and_completes_after_its_hold() {
    let mut m = Box::pin(moving()).await;
    let r = m.told.migration.as_option().unwrap();
    assert_eq!(r.role.as_known(), Some(S::MIGRATION_SIDE_DESTINATION));
    assert!(!m.told.plan.is_set(), "a standby executes no plan");
    through_verify(&mut m, ("sha256:same", "sha256:same")).await;
    assert_eq!(phase(&shown(&m.h, &m.db).await), Some(pb::MigrationPhase::MIGRATION_PHASE_CUTOVER));
    cut_over(&mut m).await;
    let now = shown(&m.h, &m.db).await;
    assert_eq!(phase(&now), Some(pb::MigrationPhase::MIGRATION_PHASE_RELEASE));
    assert!(now.hold_until.is_set(), "the hold runs from here");
    assert_eq!(records(&m.h, &m.db).await, vec![on("fra-2", "mig.data", false)], "moved");

    let release = pb::ReleaseMigrationRequest {
        mutation: mutation("release").into(),
        migration: m.migration.clone(),
        ..Default::default()
    };
    m.h.workloads(m.h.root()).release_migration(release).await.unwrap();
    let id = m.migration.clone();
    journal(
        &mut m.source,
        report(&id, (P::MIGRATION_PHASE_RELEASE, S::MIGRATION_SIDE_SOURCE), true),
    )
    .await;
    let done = shown(&m.h, &m.db).await;
    assert_eq!(done.outcome.as_known(), Some(pb::MigrationOutcome::MIGRATION_OUTCOME_COMPLETED));
    assert_eq!(plan_status(&m.h, &m.plan).await, Some(pb::PlanStatus::PLAN_STATUS_APPLIED));
}

#[tokio::test]
async fn a_rollback_after_the_move_returns_the_workload_and_keeps_what_it_wrote_there() {
    let mut m = Box::pin(moving()).await;
    through_verify(&mut m, ("sha256:same", "sha256:same")).await;
    let moved = cut_over(&mut m).await;
    let rollback = pb::RollbackMigrationRequest {
        mutation: mutation("back").into(),
        migration: m.migration.clone(),
        reason: "the new node's disk is slower".into(),
        ..Default::default()
    };
    let ended = m.h.workloads(m.h.root()).rollback_migration(rollback).await.unwrap().into_owned();
    let outcome = ended.migration.as_option().unwrap().outcome.as_known();
    assert_eq!(outcome, Some(pb::MigrationOutcome::MIGRATION_OUTCOME_ROLLED_BACK));
    let both = vec![on("fra-1", &format!("{}.data", m.db), false), on("fra-2", "mig.data", true)];
    assert_eq!(records(&m.h, &m.db).await, both, "back, and the copy kept");
    assert_eq!(plan_status(&m.h, &m.plan).await, Some(pb::PlanStatus::PLAN_STATUS_CANCELLED));
    // The destination, told the workload is no longer its, stops and gives its epoch back.
    let back = agent::LeaseReleased { workload: m.db.clone(), epoch: moved, ..Default::default() };
    up(&mut m.destination.0, Up::LeaseReleased(Box::new(back))).await;
    released(&m.h, &m.db).await;
    let epoch = held(&mut m.source.0, &m.db).await;
    assert!(epoch > moved, "the source is granted the next epoch");
}

#[tokio::test]
async fn manifests_that_differ_abandon_it_and_a_report_out_of_place_is_not_believed() {
    let mut m = Box::pin(moving()).await;
    let id = m.migration.clone();
    // The source does not prepare, and nothing is in `replicate` yet.
    journal(
        &mut m.source,
        report(&id, (P::MIGRATION_PHASE_PREPARE, S::MIGRATION_SIDE_DESTINATION), true),
    )
    .await;
    journal(
        &mut m.destination,
        report(&id, (P::MIGRATION_PHASE_REPLICATE, S::MIGRATION_SIDE_DESTINATION), true),
    )
    .await;
    assert_eq!(phase(&shown(&m.h, &m.db).await), Some(pb::MigrationPhase::MIGRATION_PHASE_PREPARE));
    assert_eq!(m.h.effects("journal.rejected").await.len(), 2, "kept, visibly");

    through_verify(&mut m, ("sha256:left", "sha256:arrived")).await;
    let ended = shown(&m.h, &m.db).await;
    assert_eq!(ended.outcome.as_known(), Some(pb::MigrationOutcome::MIGRATION_OUTCOME_ABANDONED));
    assert!(ended.reason.contains("world/"), "{}", ended.reason);
    assert_eq!(ended.volumes[0].first_difference, "world/");
    assert_eq!(plan_status(&m.h, &m.plan).await, Some(pb::PlanStatus::PLAN_STATUS_FAILED));
    assert_eq!(records(&m.h, &m.db).await, vec![on("fra-1", &format!("{}.data", m.db), false)]);
}
