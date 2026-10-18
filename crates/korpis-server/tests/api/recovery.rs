// SPDX-License-Identifier: AGPL-3.0-or-later

//! A restore recovers the declarations it lost from what nodes present (§8 of
//! docs/03-state.md): an intent declared after the backup, a workload created after it, and the
//! target moved forward to what the node runs; nothing is sent while the restore settles, so the
//! node is never told the backup's configuration; and an assignment whose project is gone is
//! refused and recorded.

use crate::agent::{Stream, TestAgent, welcome};
use crate::leases::{connect, send};
use crate::lifecycle::approve;
use crate::recipes::{body, change, create, project, push_paper, text};
use crate::support::Harness;
use korpis_app::ports::SystemClock;
use korpis_app::restore::BackupEpochs;
use korpis_app::{App, NoRecipes};
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use sqlx::Connection as _;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

fn decoded(a: &agent::SignedAssignment) -> agent::Assignment {
    <agent::Assignment as buffa::Message>::decode_from_slice(&a.assignment).unwrap()
}

/// The signed set once every one of `wanted` is in it at its intent version.
fn set_at(wanted: &[(String, u64)]) -> impl Fn(&Down) -> Option<Vec<agent::SignedAssignment>> {
    move |d| {
        let Down::AssignmentSet(s) = d else { return None };
        let at = |(w, v): &(String, u64)| {
            s.assignments.iter().map(decoded).any(|a| a.workload == *w && target(&a) == *v)
        };
        wanted.iter().all(at).then(|| s.assignments.clone())
    }
}

/// The first message `pick` accepts within thirty seconds: placing three workloads may leave
/// longer than `until` waits between two messages.
async fn within<T>(stream: &mut Stream, pick: impl Fn(&Down) -> Option<T>) -> T {
    let wait = async {
        loop {
            let m = stream.message::<agent::ControlMessage>().await.unwrap().unwrap();
            if let Some(found) = m.to_owned_message().message.as_ref().and_then(&pick) {
                return found;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(30), wait).await.expect("within thirty seconds")
}

/// The plan the set gives `workload`'s node, once it gives one.
fn plan_of(d: &Down, workload: &str) -> Option<agent::NodePlan> {
    let Down::AssignmentSet(s) = d else { return None };
    let a = s.assignments.iter().map(decoded).find(|a| a.workload == workload)?;
    a.plan.into_option()
}

fn target(a: &agent::Assignment) -> u64 {
    a.target.as_option().map_or(0, |t| t.version)
}

fn uuid(kind: IdKind, id: &str) -> Uuid {
    Id::parse(kind, id).unwrap().uuid()
}

/// Forgets, as a restore to an older backup would, intent `lost` of `workload` (whose head and
/// target go back to `kept`), and every row of each of `gone`, a workload or a project.
async fn forget(h: &Harness, workload: Uuid, (kept, lost): (Uuid, Uuid), gone: &[Uuid]) {
    let mut c = sqlx::PgConnection::connect(&h.db.owner_url).await.unwrap();
    sqlx::query("SET session_replication_role = replica").execute(&mut c).await.unwrap();
    sqlx::query(
        "UPDATE workload SET intent_id = $2, intent_version = 1, target_id = $2 WHERE id = $1",
    )
    .bind(workload)
    .bind(kept)
    .execute(&mut c)
    .await
    .unwrap();
    sqlx::query("DELETE FROM plan WHERE to_intent = $1").bind(lost).execute(&mut c).await.unwrap();
    sqlx::query("DELETE FROM intent WHERE id = $1").bind(lost).execute(&mut c).await.unwrap();
    let referencing: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.conrelid::regclass::text, a.attname::text
           FROM pg_constraint c
           JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = ANY (c.conkey)
          WHERE c.confrelid IN ('workload'::regclass, 'project'::regclass) AND c.contype = 'f'",
    )
    .fetch_all(&mut c)
    .await
    .unwrap();
    for id in gone {
        for (table, column) in &referencing {
            // Names from the catalog, never from input.
            let delete = format!("DELETE FROM {table} WHERE {column} = $1");
            sqlx::query(sqlx::AssertSqlSafe(delete)).bind(id).execute(&mut c).await.unwrap();
        }
        for table in ["workload", "project"] {
            let delete = format!("DELETE FROM {table} WHERE id = $1");
            sqlx::query(sqlx::AssertSqlSafe(delete)).bind(id).execute(&mut c).await.unwrap();
        }
    }
}

async fn effects(h: &Harness, action: &str, at_least: usize) -> usize {
    for _ in 0..50 {
        if h.effects(action).await.len() >= at_least {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    h.effects(action).await.len()
}

async fn row(h: &Harness, sql: &str, id: Uuid) -> (Uuid, i64, Option<Uuid>) {
    let store = h.db.store().await.unwrap();
    sqlx::query_as(sqlx::AssertSqlSafe(sql.to_owned()))
        .bind(id)
        .fetch_one(store.pool())
        .await
        .unwrap()
}

/// What the node ran before the restore, and what the restore then lost.
struct Ran {
    workload: String,
    lost: Uuid,
    created: Uuid,
    held: Vec<agent::SignedAssignment>,
}

/// Runs, on `agent`'s node, `w` at its second intent, `x`, and `z` in another project, takes the
/// set the node holds, then forgets `w`'s second intent, all of `x`, and `z` with its project.
async fn run_then_forget(h: &Harness, agent: &TestAgent) -> Ran {
    let mut stream = connect(h, agent, 1).await;
    let repo = push_paper().await;
    let games = project(h, &h.root_org(), "games").await;
    let gone = project(h, &h.root_org(), "gone").await;
    let workloads = h.workloads(h.root());
    let mut ids = Vec::new();
    for (key, p) in [("w", &games), ("x", &games), ("z", &gone)] {
        let r = workloads.declare_intent(create(key, p, key, body(&repo, &[]))).await.unwrap();
        ids.push(r.into_owned().workload.as_option().unwrap().id.clone());
    }
    // The test agent runs nothing, so w's creation is completed for it, or its change would
    // wait behind it.
    let created = within(&mut stream, |d| plan_of(d, &ids[0])).await;
    crate::journal::complete(&mut stream, &ids[0], &created, 1).await;
    let hard = body(&repo, &[("difficulty", text("hard"))]);
    let changed = workloads.declare_intent(change("w2", &ids[0], 1, hard)).await.unwrap();
    let plan = changed.into_owned().plan.as_option().unwrap().id.clone();
    h.plans(h.root()).approve_plan(approve("a", &plan)).await.unwrap();
    let wanted = [(ids[0].clone(), 2), (ids[1].clone(), 1), (ids[2].clone(), 1)];
    let held = within(&mut stream, set_at(&wanted)).await;
    let second = held.iter().map(decoded).find(|a| a.workload == ids[0]).unwrap();
    let lost = uuid(IdKind::Intent, &second.target.as_option().unwrap().id);
    let store = h.db.store().await.unwrap();
    let workload = uuid(IdKind::Workload, &ids[0]);
    let kept = sqlx::query_scalar("SELECT id FROM intent WHERE workload_id = $1 AND version = 1")
        .bind(workload)
        .fetch_one(store.pool())
        .await
        .unwrap();
    let created = uuid(IdKind::Workload, &ids[1]);
    let gone = [created, uuid(IdKind::Workload, &ids[2]), uuid(IdKind::Project, &gone)];
    forget(h, workload, (kept, lost), &gone).await;
    Ran { workload: ids[0].clone(), lost, created, held }
}

async fn recovered_from(h: &Harness, intent: Uuid) -> serde_json::Value {
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar("SELECT recovered_from FROM intent WHERE id = $1")
        .bind(intent)
        .fetch_one(store.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_restore_recovers_what_the_nodes_run_and_tells_them_nothing_older() {
    let h = Harness::new().await;
    let node = TestAgent::enroll(&h, "fra-3", 1).await;
    let ran = run_then_forget(&h, &node).await;
    let app = App::new(h.db.store().await.unwrap(), SystemClock, test_key(), NoRecipes);
    let backup = BackupEpochs { max_issued: 0, rate_per_sec: 0, taken_at: SystemTime::now() };
    let fenced = app.fence_epochs(backup, Duration::from_secs(4), "uid 0 (root)").await.unwrap();
    let (mut stream, first) = TestAgent::open(&h, node.hello_at(SystemTime::now(), &[2; 16])).await;
    assert!(welcome(&first.unwrap()).present_assignments);
    let presented = agent::Presented { assignments: ran.held.clone(), ..Default::default() };
    send(&mut stream, Up::Presented(Box::new(presented.clone()))).await;

    assert_eq!(effects(&h, "restore.workload.recovered", 1).await, 1, "x, and not z");
    assert_eq!(effects(&h, "restore.intent.recovered", 2).await, 2, "w's second and x's first");
    assert_eq!(effects(&h, "restore.recovery.refused", 1).await, 1, "z's project is gone");
    assert_eq!(effects(&h, "restore.power.recovered", 1).await, 1, "x's");
    assert_eq!(effects(&h, "restore.targets.recovered", 2).await, 2, "w's and x's");
    let state = "SELECT intent_id, intent_version, target_id FROM workload WHERE id = $1";
    let lost = ran.lost;
    let w = uuid(IdKind::Workload, &ran.workload);
    assert_eq!(row(&h, state, w).await, (lost, 2, Some(lost)), "w runs its second intent again");
    let (intent, version, target_id) = row(&h, state, ran.created).await;
    assert_eq!((version, target_id), (1, Some(intent)), "x is back, at what it runs");
    let from = recovered_from(&h, lost).await;
    assert_eq!(from["node"], uuid(IdKind::Node, &node.node).to_string(), "{from}");

    send(&mut stream, Up::Presented(Box::new(presented))).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(h.effects("restore.intent.recovered").await.len(), 2, "recovered once");

    // Nothing was sent while it settled; once it ends, the set names what the node runs.
    let told = |d: &Down| matches!(d, Down::AssignmentSet(_) | Down::InSync(_)).then(|| d.clone());
    let told = within(&mut stream, told).await;
    assert!(SystemTime::now() >= fenced.settling_until, "not before the window ends");
    if let Down::AssignmentSet(s) = told {
        let now = s.assignments.iter().map(decoded).find(|a| a.workload == ran.workload).unwrap();
        assert_eq!(target(&now), 2, "never the backup's configuration");
    }
}

fn test_key() -> std::sync::Arc<korpis_core::authz::token::SigningKey> {
    std::sync::Arc::new(korpis_core::authz::token::SigningKey::generate(9))
}
