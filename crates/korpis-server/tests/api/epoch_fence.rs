// SPDX-License-Identifier: AGPL-3.0-or-later

//! A restore's first fence (§8 of docs/03-state.md): the floor past what the rate allowed since
//! the backup, a settling window in which no epoch is issued and each reconnecting agent is asked
//! to present what it holds, and a floor raised by the epochs that verify, never by those that
//! do not.

use crate::agent::{TestAgent, welcome};
use crate::leases::{connect, grant, refusal, request, send, until};
use crate::recipes::{body, create, project, push_paper};
use crate::support::Harness;
use korpis_app::ports::SystemClock;
use korpis_app::restore::BackupEpochs;
use korpis_app::{App, NoRecipes};
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::agent::v1::__buffa::oneof::agent_message::Message as Up;
use korpis_proto::korpis::agent::v1::__buffa::oneof::control_message::Message as Down;
use std::time::{Duration, SystemTime};

/// The node's signed assignments, once a set carries a lease epoch.
fn leased_set(d: &Down) -> Option<Vec<agent::SignedAssignment>> {
    let Down::AssignmentSet(s) = d else { return None };
    let first = s.assignments.first()?;
    let a = <agent::Assignment as buffa::Message>::decode_from_slice(&first.assignment).ok()?;
    (a.lease_epoch > 0).then(|| s.assignments.clone())
}

/// Forgeries of `held`: the real bytes with a signature of zeros, and an assignment claiming a far
/// higher epoch than `epoch` under the real signature, which no longer covers its bytes.
fn forged(held: &[agent::SignedAssignment], epoch: u64) -> Vec<agent::SignedAssignment> {
    let mut forged = held.to_vec();
    for f in &mut forged {
        f.signature = vec![0; 64];
    }
    let mut higher = held[0].clone();
    let mut claimed =
        <agent::Assignment as buffa::Message>::decode_from_slice(&higher.assignment).unwrap();
    claimed.lease_epoch = epoch + 1_000;
    higher.assignment = buffa::Message::encode_to_vec(&claimed);
    forged.push(higher);
    forged
}

async fn floor(h: &Harness) -> i64 {
    let store = h.db.store().await.unwrap();
    sqlx::query_scalar("SELECT floor FROM epoch_state").fetch_one(store.pool()).await.unwrap()
}

#[tokio::test]
async fn a_settling_restore_issues_nothing_and_raises_its_floor_by_verified_epochs_alone() {
    let h = Harness::new().await;
    let a = TestAgent::enroll(&h, "fra-3", 1).await;
    let mut sa = connect(&h, &a, 1).await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declare = |name: &str| create(name, &project, name, body(&repo, &[]));
    let w = h.workloads(h.root()).declare_intent(declare("w")).await.unwrap().into_owned();
    let workload = w.workload.as_option().unwrap().id.clone();
    send(&mut sa, request(&workload, 1)).await;
    let epoch = until(&mut sa, grant).await.epoch;
    let held = until(&mut sa, leased_set).await;
    let store = h.db.store().await.unwrap();
    let uuid = korpis_core::id::Id::parse(korpis_core::id::IdKind::Node, &a.node).unwrap().uuid();
    let node = format!("lease:{uuid}");
    let charged: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_bucket WHERE key = $1")
        .bind(&node)
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(charged, 1, "the issue spent from the node's own bucket first");

    // Presented with no restore settling: nothing asked for it, so it raises nothing.
    let unasked = agent::Presented { assignments: held.clone(), ..Default::default() };
    send(&mut sa, Up::Presented(Box::new(unasked))).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(floor(&h).await, 0, "ignored outside a restore");
    drop(sa);

    let other = h.workloads(h.root()).declare_intent(declare("v")).await.unwrap().into_owned();
    let other = other.workload.as_option().unwrap().id.clone();

    // Restored to a backup that knew no epoch, at a rate of none: the floor the arithmetic gives
    // is the watermark itself, so only what the node presents can raise it.
    let app = App::new(h.db.store().await.unwrap(), SystemClock, test_key(), NoRecipes);
    let backup = BackupEpochs { max_issued: 0, rate_per_sec: 0, taken_at: SystemTime::now() };
    let fenced = app.fence_epochs(backup, Duration::from_secs(3), "uid 0 (root)").await.unwrap();
    assert_eq!(fenced.floor, 0);

    let (mut sb, first) = TestAgent::open(&h, a.hello_at(SystemTime::now(), &[2; 16])).await;
    assert!(welcome(&first.unwrap()).present_assignments, "asked to present while settling");
    let forged = forged(&held, epoch);
    let presented = agent::Presented { assignments: [forged, held].concat(), ..Default::default() };
    send(&mut sb, Up::Presented(Box::new(presented))).await;
    for _ in 0..50 {
        if floor(&h).await == i64::try_from(epoch).unwrap() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(floor(&h).await, i64::try_from(epoch).unwrap(), "raised to the signed epoch");
    assert_eq!(h.effects("restore.epoch_floor.raised").await.len(), 1);

    // What the API last asked of the store is kept for a second; the fence came within it.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let refused_call = h.workloads(h.root()).declare_intent(declare("u")).await.unwrap_err();
    assert_eq!(refused_call.code, connectrpc::ErrorCode::Unavailable, "no caller while it settles");
    send(&mut sb, request(&other, 2)).await;
    let refused = until(&mut sb, refusal).await;
    assert_eq!(
        refused.reason.as_known(),
        Some(agent::LeaseRefusalReason::LEASE_REFUSAL_REASON_UNAVAILABLE),
        "nothing is issued while it settles"
    );

    tokio::time::sleep(Duration::from_secs(3)).await;
    send(&mut sb, request(&other, 3)).await;
    let after = until(&mut sb, grant).await;
    assert!(after.epoch > epoch, "past every epoch a node proved: {} > {epoch}", after.epoch);
    h.workloads(h.root()).declare_intent(declare("u")).await.expect("served once it settled");
}

fn test_key() -> std::sync::Arc<korpis_core::authz::token::SigningKey> {
    std::sync::Arc::new(korpis_core::authz::token::SigningKey::generate(9))
}
