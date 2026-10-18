// SPDX-License-Identifier: AGPL-3.0-or-later

//! A restore recovers the runs its nodes are running from the assignments they present (§8 of
//! docs/03-state.md): a run the backup lacks is recorded running on the node that presented it,
//! a run the backup holds as pending is started there, and the run maker makes nothing while
//! the restore settles, so it never makes a second run of what a node is running.

use crate::agent::{Stream, TestAgent, welcome};
use crate::leases::{connect, send};
use crate::recipes::{body, create, project, push_paper};
use crate::recovery::{decoded, effects, test_key, within};
use crate::support::Harness;
use korpis_app::ports::SystemClock;
use korpis_app::restore::BackupEpochs;
use korpis_app::{App, NoRecipes};
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use sqlx::Connection as _;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// A task placed on `node`'s node, its first run made and named: the workload, the run, and the
/// set the node holds.
async fn named_run(h: &Harness, node: &TestAgent) -> (Uuid, Uuid, Vec<agent::SignedAssignment>) {
    let mut stream = connect(h, node, 1).await;
    let repo = push_paper().await;
    let jobs = project(h, &h.root_org(), "jobs").await;
    let task =
        pb::IntentBody { lifecycle: pb::Lifecycle::LIFECYCLE_TASK.into(), ..body(&repo, &[]) };
    let declared = h.workloads(h.root()).declare_intent(create("t", &jobs, "build", task)).await;
    let workload = declared.unwrap().into_owned().workload.into_option().unwrap().id;
    let held = within(&mut stream, |d| match d {
        Down::AssignmentSet(s) if s.assignments.iter().any(|a| !decoded(a).runs.is_empty()) => {
            Some(s.assignments.clone())
        }
        _ => None,
    })
    .await;
    let run = decoded(&held[0]).runs[0].run.clone();
    let id = |kind, s: &str| Id::parse(kind, s).unwrap().uuid();
    (id(IdKind::Workload, &workload), id(IdKind::Run, &run), held)
}

/// Runs `sql` on `workload` as the owner, as a restore to an older backup would leave it.
async fn rewind(h: &Harness, sql: &[&str], workload: Uuid) {
    let mut c = sqlx::PgConnection::connect(&h.db.owner_url).await.unwrap();
    for statement in sql {
        let q = sqlx::query(sqlx::AssertSqlSafe((*statement).to_owned()));
        q.bind(workload).execute(&mut c).await.unwrap();
    }
}

/// Fences as a restore does, checks that the run maker makes nothing while it settles, and
/// presents `held` from `node`; the session, held open so the presentation is read.
async fn restore_and_present(
    h: &Harness,
    node: &TestAgent,
    held: Vec<agent::SignedAssignment>,
) -> Stream {
    let app = App::new(h.db.store().await.unwrap(), SystemClock, test_key(), NoRecipes);
    let backup = BackupEpochs { max_issued: 0, rate_per_sec: 0, taken_at: SystemTime::now() };
    app.fence_epochs(backup, Duration::from_secs(4), "uid 0 (root)").await.unwrap();
    assert_eq!(app.tend_runs().await.unwrap(), 0, "no run is made or ended while it settles");
    let (mut stream, first) = TestAgent::open(h, node.hello_at(SystemTime::now(), &[2; 16])).await;
    assert!(welcome(&first.unwrap()).present_assignments);
    let presented = agent::Presented { assignments: held, ..Default::default() };
    send(&mut stream, Up::Presented(Box::new(presented))).await;
    stream
}

async fn runs_of(h: &Harness, workload: Uuid) -> Vec<(Uuid, String, Option<Uuid>, Option<i64>)> {
    let store = h.db.store().await.unwrap();
    sqlx::query_as("SELECT id, status, node_id, cycle FROM run WHERE workload_id = $1")
        .bind(workload)
        .fetch_all(store.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_run_the_backup_lacks_is_recovered_running_where_it_runs_and_made_once() {
    let h = Harness::new().await;
    let node = TestAgent::enroll(&h, "fra-3", 1).await;
    let (workload, run, held) = named_run(&h, &node).await;
    let forget =
        ["DELETE FROM run WHERE workload_id = $1", "DELETE FROM run_state WHERE workload_id = $1"];
    rewind(&h, &forget, workload).await;
    let _session = restore_and_present(&h, &node, held).await;

    assert_eq!(effects(&h, "restore.run.recovered", 1).await, 1);
    let on = Id::parse(IdKind::Node, &node.node).unwrap().uuid();
    let recovered = runs_of(&h, workload).await;
    assert_eq!(recovered, [(run, "running".to_owned(), Some(on), Some(0))]);
    let store = h.db.store().await.unwrap();
    let made: Option<i64> =
        sqlx::query_scalar("SELECT made_cycle FROM run_state WHERE workload_id = $1")
            .bind(workload)
            .fetch_one(store.pool())
            .await
            .unwrap();
    assert_eq!(made, Some(0), "its cycle is made, so no second run is made for it");
    assert_eq!(h.effects("restore.run.refused").await.len(), 0);
}

#[tokio::test]
async fn a_run_the_backup_holds_pending_is_started_where_it_runs() {
    let h = Harness::new().await;
    let node = TestAgent::enroll(&h, "fra-3", 1).await;
    let (workload, run, held) = named_run(&h, &node).await;
    rewind(&h, &["UPDATE run SET status = 'pending' WHERE workload_id = $1"], workload).await;
    let _session = restore_and_present(&h, &node, held).await;

    assert_eq!(effects(&h, "restore.run.started", 1).await, 1);
    let on = Id::parse(IdKind::Node, &node.node).unwrap().uuid();
    assert_eq!(runs_of(&h, workload).await, [(run, "running".to_owned(), Some(on), Some(0))]);
    assert_eq!(h.effects("restore.run.recovered").await.len(), 0);
}

#[tokio::test]
async fn a_run_the_store_already_went_past_is_not_recorded_again() {
    let h = Harness::new().await;
    let node = TestAgent::enroll(&h, "fra-3", 1).await;
    let (workload, _, held) = named_run(&h, &node).await;
    // Ended and pruned before the backup, whose run state still says its cycle was made: what
    // the node presents is an older assignment than what the store holds.
    rewind(&h, &["DELETE FROM run WHERE workload_id = $1"], workload).await;
    let _session = restore_and_present(&h, &node, held).await;

    assert_eq!(effects(&h, "restore.run.refused", 1).await, 1);
    assert!(runs_of(&h, workload).await.is_empty(), "nothing recorded running");
    assert_eq!(h.effects("restore.run.recovered").await.len(), 0);
}
