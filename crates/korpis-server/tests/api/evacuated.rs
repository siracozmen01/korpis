// SPDX-License-Identifier: AGPL-3.0-or-later

//! An evacuation of a node that fell silent. A drain is refused there, since the node cannot take
//! part. Proposed, the evacuation says per workload where each volume's data comes from, here
//! nowhere, its only copy lost with the node, and that the workload may still be running there.
//! Approved, the workload is placed on the other node by a remake plan naming the operation, which
//! makes its volume anew rather than attaching the lost one; the lost volume is left behind as an
//! orphan, and the move is recorded as the approver's evacuation.

use crate::agent::{TestAgent, welcome};
use crate::drained::{approved, made, proposed, tended};
use crate::failover::kind;
use crate::journal::{assignment, until};
use crate::migrating::local;
use crate::recipes::project;
use crate::support::Harness;
use connectrpc::ErrorCode;
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::v1 as pb;
use std::time::SystemTime;

#[tokio::test]
async fn an_evacuation_names_what_is_lost_and_makes_the_workload_anew_elsewhere() {
    let h = Harness::new().await;
    let (a, b) = (TestAgent::enroll(&h, "fra-1", 1).await, TestAgent::enroll(&h, "fra-2", 2).await);
    let (mut sa, first) =
        TestAgent::open(&h, a.hello_with(SystemTime::now(), &[1; 16], local())).await;
    welcome(&first.unwrap());
    let project = project(&h, &h.root_org(), "games").await;
    let (db, _) = made(&h, &mut sa, (&project, "db"), 1).await;
    let (mut sb, first) =
        TestAgent::open(&h, b.hello_with(SystemTime::now(), &[2; 16], local())).await;
    welcome(&first.unwrap());
    // The power is cut: nothing more is heard from the first node, and the sweep finds it silent.
    drop(sa);
    let store = h.db.store().await.unwrap();
    let silent = "UPDATE node SET last_seen = now() - interval '1 hour' WHERE name = 'fra-1'";
    sqlx::query(silent).execute(store.pool()).await.unwrap();
    h.api.app().sweep_silent_nodes().await.unwrap();

    let policy = pb::OperationPolicy::default();
    let drain = proposed(&h, (pb::OperationKind::OPERATION_KIND_DRAIN, &a.node), policy).await;
    assert_eq!(drain.unwrap_err().code, ErrorCode::Aborted, "it cannot take part");
    let evacuate = (pb::OperationKind::OPERATION_KIND_EVACUATE, a.node.as_str());
    let op = proposed(&h, evacuate, pb::OperationPolicy::default()).await.unwrap();
    let said = "SELECT detail->'volumes'->0->>'from', (detail->>'may_still_run')::bool
                  FROM operation_item WHERE operation_id = $1";
    let id = Id::parse(IdKind::Operation, &op.id).unwrap().uuid();
    let (from, may_run): (String, bool) =
        sqlx::query_as(said).bind(id).fetch_one(store.pool()).await.unwrap();
    assert_eq!(from, "empty", "its only copy was on the node");
    assert!(may_run, "kept running when cut off, it may be running still");

    approved(&h, &op.id, "").await;
    let begun = tended(&h, &op.id).await;
    let item = &begun.items[0];
    assert_eq!(item.state.as_known(), Some(pb::OperationItemState::OPERATION_ITEM_STATE_APPLYING));
    let told = until(&mut sb, assignment(|t| t.workload == db && t.plan.is_set())).await;
    let plan = told.plan.as_option().unwrap();
    assert_eq!(kind(plan), Some(pb::PlanKind::PLAN_KIND_REMAKE));
    let actions: Vec<&str> = plan.steps.iter().map(|s| s.action.as_str()).collect();
    assert_eq!(actions, ["pull_recipe", "create_volume", "create", "start"], "made anew");
    let left = "SELECT count(*) FROM volume WHERE name = 'data' AND attached_to IS NULL";
    let orphans: i64 = sqlx::query_scalar(left).fetch_one(store.pool()).await.unwrap();
    assert_eq!(orphans, 1, "the lost volume is left behind, never deleted");
    let recorded = h.effects("workload.evacuate").await;
    assert_eq!(recorded.len(), 1, "{recorded:?}");
}
