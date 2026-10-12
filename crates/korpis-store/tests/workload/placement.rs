// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placements and the record of failed attempts: stored whole, summed per node, logged once
//! per target (§2.3 of docs/05-scheduling.md).

use crate::support::{create, fixture, now};
use korpis_app::Page;
use korpis_app::ports::{PlacementRead, PlacementWrite, Store, Tx, WorkloadWrite};
use korpis_core::labels::LabelSelector;
use korpis_core::placement::{BoundBy, Placement, Unplaced};
use korpis_core::schedule::{
    Check, Explanation, PlacementPolicy, Rejection, Score, Signal, SpreadConstraint, SpreadScope,
    Strategy,
};
use korpis_core::tenancy::quota::{Bounds, Dimension};
use korpis_store::testing::{enroll, ready_node};
use uuid::Uuid;

fn explanation(node: Uuid) -> Explanation {
    let mut policy = PlacementPolicy { strategy: Strategy::Packed, ..Default::default() };
    policy.spread.push(SpreadConstraint {
        topology_key: "rack".into(),
        max_skew: 1,
        scope: SpreadScope::Selector(LabelSelector {
            match_labels: [("app".to_owned(), "db".to_owned())].into(),
            ..Default::default()
        }),
    });
    policy.weights.insert(Signal::FailureDomain, 2.5);
    Explanation {
        candidates: 2,
        filtered: vec![Rejection { node: Uuid::now_v7(), check: Check::Tier, detail: "vm".into() }],
        scored: vec![Score { node, signal: Signal::Utilization, value: 0.25, weight: 1.0 }],
        chosen: Some(node),
        runner_up: None,
        policy,
    }
}

#[tokio::test]
async fn a_placement_reads_back_whole_and_its_node_counts_it() {
    let f = fixture().await;
    let (w, first) = create(&f, "mc").await;
    let node = ready_node(f.organization, "fra-3", &[]).unwrap();
    enroll(&f.store, &node).await.unwrap();
    let p = Placement {
        workload: w.id,
        node: node.id,
        intent: first.id,
        reserved: Bounds::from([(Dimension::Memory, 4 << 30), (Dimension::Cpu, 2_000)]),
        bound_at: now(),
        bound_by: BoundBy::Scheduler,
        reason: "best".into(),
        explanation: explanation(node.id),
        authoritative: true,
    };
    let mut tx = f.store.begin().await.unwrap();
    tx.move_target(w.id, first.id, 0).await.unwrap();
    tx.lock_placement().await.unwrap();
    tx.bind(&p).await.unwrap();
    assert_eq!(tx.allocations(&[node.id]).await.unwrap()[&node.id], p.reserved);
    tx.commit().await.unwrap();
    assert_eq!(f.store.placement(w.id).await.unwrap(), Some(p));
    assert!(f.store.awaiting_placement(Page::default()).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_attempt_is_new_once_per_target() {
    let f = fixture().await;
    let (w, first) = create(&f, "mc").await;
    let mut tx = f.store.begin().await.unwrap();
    tx.move_target(w.id, first.id, 0).await.unwrap();
    let u = Unplaced {
        workload: w.id,
        intent: first.id,
        since: now(),
        explanation: explanation(Uuid::now_v7()),
    };
    assert!(tx.record_unplaced(&u).await.unwrap(), "the first attempt for this target");
    assert!(!tx.record_unplaced(&u).await.unwrap(), "a retry of the same target");
    tx.commit().await.unwrap();
    assert_eq!(f.store.unplaced(w.id).await.unwrap(), Some(u));
    assert_eq!(f.store.awaiting_placement(Page::default()).await.unwrap(), [w.id]);
    let mut tx = f.store.begin().await.unwrap();
    tx.unbind(w.id).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(f.store.unplaced(w.id).await.unwrap(), None);
}
