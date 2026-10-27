// SPDX-License-Identifier: AGPL-3.0-or-later

//! Failover (§2.4 of docs/05-scheduling.md): a node falls silent holding two workloads that stop
//! when their lease ends. Once the sweep marks it unreachable and the grantor's end of the lease
//! has passed, the one whose volume is in a shared store the other node offers is placed there,
//! by failover, and made there by a remake plan the control plane approved, which attaches its
//! volume rather than make it; the one whose volume is on the silent node's own disk is not
//! moved, and its status says why. A workload made on a node removed as compromised is made
//! again where it lands (Finding 204).

use crate::agent::{TestAgent, welcome};
use crate::journal::{ack, complete, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::streams::held;
use crate::support::Harness;
use crate::volume_shared::{classes, on_shared};
use crate::volumes::{ensured, volume_record};
use buffa::Message as _;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use korpis_proto::korpis::v1 as pb;
use std::time::{Duration, Instant, SystemTime};

async fn status(h: &Harness, workload: &str) -> pb::WorkloadStatus {
    let get = pb::GetWorkloadRequest { id: workload.to_owned(), ..Default::default() };
    let got = h.workloads(h.root()).get_workload(get).await.unwrap().into_owned();
    got.status.into_option().unwrap()
}

/// Any assignment of the next set that satisfies `pick`.
pub(crate) fn assignment(
    pick: impl Fn(&agent::Assignment) -> bool,
) -> impl Fn(&Down) -> Option<agent::Assignment> {
    move |d| {
        let Down::AssignmentSet(s) = d else { return None };
        s.assignments
            .iter()
            .filter_map(|a| agent::Assignment::decode_from_slice(&a.assignment).ok())
            .find(|a| pick(a))
    }
}

/// The example's declaration with its `data` volume on the node's own disk, stopping when its
/// lease ends.
fn on_local(repo: &str) -> pb::IntentBody {
    let mut b = body(repo, &[]);
    b.volumes = vec![pb::VolumeSpec {
        name: "data".into(),
        storage_class: "local".into(),
        ..Default::default()
    }];
    b.continuity = pb::ContinuitySpec {
        on_lease_expiry: pb::OnLeaseExpiry::ON_LEASE_EXPIRY_STOP.into(),
        ..Default::default()
    }
    .into();
    b
}

/// The kind of the plan a node is told to execute.
pub(crate) fn kind(plan: &agent::NodePlan) -> Option<pb::PlanKind> {
    plan.whole.as_option().and_then(|w| w.kind.as_known())
}

/// Declares `name` as `b`; its identifier.
async fn declared(h: &Harness, project: &str, name: &str, b: pb::IntentBody) -> String {
    let workloads = h.workloads(h.root());
    let made = workloads.declare_intent(create(name, project, name, b)).await.unwrap();
    made.into_owned().workload.into_option().unwrap().id
}

/// Keeps the node `name` heard, as its heartbeats would, until the task is aborted.
pub(crate) fn keep_heard(h: &Harness, name: &'static str) -> tokio::task::JoinHandle<()> {
    let url = h.db.owner_url.clone();
    tokio::spawn(async move {
        let pool = sqlx::PgPool::connect(&url).await.unwrap();
        let heard = "UPDATE node SET last_seen = now() + interval '1 hour' WHERE name = $1";
        loop {
            let _ = sqlx::query(heard).bind(name).execute(&pool).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
}

#[tokio::test]
async fn a_workload_whose_node_is_gone_is_placed_again_where_its_data_is() {
    let h = Harness::with_heartbeat(Duration::from_millis(300)).await;
    let (a, b) = (TestAgent::enroll(&h, "fra-1", 1).await, TestAgent::enroll(&h, "fra-2", 2).await);
    let heard_a = keep_heard(&h, "fra-1");
    let (mut sa, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], classes())).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;

    let db = declared(&h, &project, "db", on_shared(&repo, None)).await;
    let told = until(&mut sa, assignment(|t| t.workload == db && t.plan.is_set())).await;
    let last = complete(&mut sa, &db, told.plan.as_option().unwrap(), 1).await;
    // A node reports only the volumes of a workload whose lease it held.
    held(&mut sa, &db).await;
    let leased = Instant::now();
    let mut reported = ensured(&db, "data", false, false);
    reported.storage_class = "shared".into();
    send(&mut sa, vec![volume_record(last + 1, reported)]).await;
    assert_eq!(until(&mut sa, ack).await, last + 1);
    let cache = declared(&h, &project, "cache", on_local(&repo)).await;
    let told = until(&mut sa, assignment(|t| t.workload == cache && t.plan.is_set())).await;
    let last = complete(&mut sa, &cache, told.plan.as_option().unwrap(), last + 2).await;
    held(&mut sa, &cache).await;
    let mut reported = ensured(&cache, "data", false, false);
    reported.storage_class = "local".into();
    send(&mut sa, vec![volume_record(last + 1, reported)]).await;
    assert_eq!(until(&mut sa, ack).await, last + 1);

    keep_heard(&h, "fra-2");
    let (mut sb, first) =
        TestAgent::open(&h, b.hello_with(SystemTime::now(), &[2; 16], classes())).await;
    welcome(&first.unwrap());
    // The power is cut: nothing more is heard from the first node.
    heard_a.abort();
    drop(sa);
    let store = h.db.store().await.unwrap();
    let silent = "UPDATE node SET last_seen = now() - interval '1 hour' WHERE name = 'fra-1'";
    sqlx::query(silent).execute(store.pool()).await.unwrap();
    let gone = "SELECT count(*) FROM node WHERE name = 'fra-1' AND connectivity = 'unreachable'";
    let moved = until(&mut sb, assignment(|t| t.workload == db)).await;
    assert!(leased.elapsed() >= Duration::from_millis(1800), "not before its lease ended");
    let plan = moved.plan.as_option().expect("a plan making it there");
    assert_eq!(kind(plan), Some(pb::PlanKind::PLAN_KIND_REMAKE), "the control plane's");
    let actions: Vec<&str> = plan.steps.iter().map(|s| s.action.as_str()).collect();
    assert_eq!(actions, ["pull_recipe", "attach_volume", "create", "start"], "made, not new");
    let marked: i64 = sqlx::query_scalar(gone).fetch_one(store.pool()).await.unwrap();
    assert_eq!(marked, 1, "moved from a node the sweep marked unreachable");

    let placed = status(&h, &db).await.placement.into_option().unwrap();
    assert_eq!(placed.node, b.node);
    assert_eq!(placed.bound_by.as_known(), Some(pb::BoundBy::BOUND_BY_FAILOVER));
    let effects = h.effects("workload.failover").await;
    assert_eq!(effects.len(), 1, "one failover, recorded: {effects:?}");

    // Its only copy is on the node that is gone: it stays, and says why once its own lease, taken
    // after the other's, has ended too.
    let mut kept = status(&h, &cache).await;
    for _ in 0..100 {
        if kept.unplaced.is_set() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        kept = status(&h, &cache).await;
    }
    assert_eq!(kept.placement.as_option().map(|p| p.node.clone()), Some(a.node.clone()));
    let unplaced = kept.unplaced.into_option().expect("its status says no node can take it");
    let why = format!("{:?}", unplaced.explanation);
    assert!(why.contains("volume"), "its data is on the node that is gone: {why}");
}

#[tokio::test]
async fn a_workload_made_on_a_node_removed_as_compromised_is_made_again_where_it_lands() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-1", 1).await;
    let (mut sa, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[1; 16])).await;
    welcome(&first.unwrap());
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let paper = declared(&h, &project, "paper", body(&repo, &[])).await;
    let told = until(&mut sa, assignment(|t| t.workload == paper && t.plan.is_set())).await;
    complete(&mut sa, &paper, told.plan.as_option().unwrap(), 1).await;
    let b = TestAgent::enroll(&h, "fra-2", 2).await;
    let (mut sb, first) = TestAgent::open(&h, b.hello_at(SystemTime::now(), &[2; 16])).await;
    welcome(&first.unwrap());

    let removal = pb::RemoveNodeRequest {
        mutation: crate::support::mutation("remove-a").into(),
        node: a.node.clone(),
        compromised: true,
        ..Default::default()
    };
    h.nodes(h.root()).remove_node(removal).await.unwrap();
    let landed = until(&mut sb, assignment(|t| t.workload == paper && t.plan.is_set())).await;
    let plan = landed.plan.as_option().unwrap();
    assert_eq!(kind(plan), Some(pb::PlanKind::PLAN_KIND_REMAKE), "a plan of its own");
    let actions: Vec<&str> = plan.steps.iter().map(|s| s.action.as_str()).collect();
    // Its volume was never reported made, so it is made where it lands.
    let made = ["pull_recipe", "create_volume", "create", "start"];
    assert_eq!(actions, made, "its content fetched where it lands");
    let placed = status(&h, &paper).await.placement.into_option().unwrap();
    assert_eq!(placed.node, b.node);
}
