// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placement (§2 of docs/05-scheduling.md): bound when the target is approved, retried while no
//! node can take it, never moved by an approval, and explained to each viewer as far as their
//! grants reach (§8 of docs/08-identity.md).

use crate::lifecycle::{approve, workload};
use crate::recipes::{body, change};
use crate::support::{Harness, mutation};
use korpis_core::authz::{Kind, ObjectRef, Scope};
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::{principal, scope};
use std::time::{Duration, Instant};

const GIB: u64 = 1 << 30;

async fn status(
    h: &Harness,
    config: connectrpc::client::ClientConfig,
    w: &str,
) -> pb::WorkloadStatus {
    let workloads = h.workloads(config);
    let got =
        workloads.get_workload(pb::GetWorkloadRequest { id: w.to_owned(), ..Default::default() });
    got.await.unwrap().into_owned().status.into_option().unwrap()
}

/// The status once `done` holds, polling while the retry pass runs; panics after five seconds.
async fn until(
    h: &Harness,
    w: &str,
    done: impl Fn(&pb::WorkloadStatus) -> bool,
) -> pb::WorkloadStatus {
    let start = Instant::now();
    loop {
        let s = status(h, h.root(), w).await;
        if done(&s) {
            return s;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "never happened: {s:?}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn placed(s: &pb::WorkloadStatus) -> bool {
    s.placement.is_set()
}

fn outcomes(effects: &[(String, Option<uuid::Uuid>)]) -> Vec<&str> {
    effects.iter().map(|(o, _)| o.as_str()).collect()
}

#[tokio::test]
async fn an_approved_workload_lands_on_a_node_and_says_why() {
    let h = Harness::new().await;
    let node = h.node(None, "fra-3", &[("region", "eu")]).await;
    let (w, _) = workload(&h).await;
    let st = status(&h, h.root(), &w).await;
    let placement = st.placement.as_option().unwrap();
    assert_eq!((placement.node.as_str(), placement.region.as_str()), (node.as_str(), "eu"));
    assert_eq!(placement.tier.as_known(), Some(pb::IsolationTier::ISOLATION_TIER_CONTAINER));
    let why = placement.explanation.as_option().unwrap();
    assert_eq!((why.candidates, why.chosen.as_str()), (Some(1), node.as_str()));
    assert!(why.scored.iter().any(|score| score.signal == "utilization"));
    assert_ne!(st.convergence.as_known(), Some(pb::Convergence::CONVERGENCE_UNPLACED));
    assert!(!st.unplaced.is_set());
    let nodes = h.nodes(h.root());
    let got = nodes.get_node(pb::GetNodeRequest { id: node, ..Default::default() });
    let allocated = got.await.unwrap().into_owned().node.into_option().unwrap().allocated;
    let held = allocated.as_option().unwrap().memory_bytes;
    assert!(held >= GIB, "the reservation is held");
    assert_eq!(placement.reserved.memory_bytes, held, "and the placement says what it holds");
    assert_eq!(outcomes(&h.effects("workload.place").await), ["succeeded"]);
}

#[tokio::test]
async fn with_no_node_it_waits_unplaced_and_lands_when_one_arrives() {
    let h = Harness::new().await;
    let (w, _) = workload(&h).await;
    let s = status(&h, h.root(), &w).await;
    assert_eq!(s.convergence.as_known(), Some(pb::Convergence::CONVERGENCE_UNPLACED));
    let why = s.unplaced.as_option().unwrap().explanation.as_option().unwrap();
    assert_eq!(why.candidates, Some(0));
    // Several retry passes run; the failure is logged once for this target, not once per pass.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(outcomes(&h.effects("workload.place").await), ["failed"]);
    h.node(None, "fra-3", &[]).await;
    let s = until(&h, &w, placed).await;
    assert!(!s.unplaced.is_set(), "a bind forgets the failure");
    let placed = h.effects("workload.place").await;
    assert_eq!(outcomes(&placed), ["failed", "succeeded"]);
    let named = placed.iter().all(|(_, grant)| grant.is_some());
    assert!(named, "the scheduler acts under the grant of the decision it carries out");
}

#[tokio::test]
async fn a_cordoned_node_takes_nothing_new_until_uncordoned() {
    let h = Harness::new().await;
    let node = h.node(None, "fra-3", &[]).await;
    let cordon = pb::CordonNodeRequest {
        mutation: mutation("c").into(),
        node: node.clone(),
        ..Default::default()
    };
    h.nodes(h.root()).cordon_node(cordon).await.unwrap();
    let (w, _) = workload(&h).await;
    let s = status(&h, h.root(), &w).await;
    let why = s.unplaced.as_option().unwrap().explanation.as_option().unwrap();
    assert_eq!(why.filtered[0].check, "status");
    assert_eq!((why.rejected[0].check.as_str(), why.rejected[0].nodes), ("status", Some(1)));
    let uncordon = pb::UncordonNodeRequest {
        mutation: mutation("u").into(),
        node: node.clone(),
        ..Default::default()
    };
    h.nodes(h.root()).uncordon_node(uncordon).await.unwrap();
    let s = until(&h, &w, placed).await;
    assert_eq!(s.placement.as_option().unwrap().node, node);
}

async fn reader(h: &Harness, key: &str, scope: Scope) -> connectrpc::client::ClientConfig {
    let (token, secret) = h.bare_token().await;
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation(key).into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: vec!["workload.read".into()],
            scope: scope::to_pb(&scope).into(),
            ..Default::default()
        })
        .await
        .unwrap();
    h.config(Some(secret.expose()))
}

#[tokio::test]
async fn each_viewer_sees_as_far_as_their_grants_reach() {
    let h = Harness::new().await;
    h.node(None, "fra-3", &[("region", "eu")]).await;
    let (w, _) = workload(&h).await;
    let org = Id::parse(IdKind::Organization, &h.root_org()).unwrap().uuid();
    let tenant = reader(&h, "t", Scope::Object(ObjectRef::new(Kind::Organization, org))).await;
    let id = Id::parse(IdKind::Workload, &w).unwrap().uuid();
    let owner = reader(&h, "o", Scope::Object(ObjectRef::new(Kind::Workload, id))).await;

    let seen = status(&h, tenant, &w).await.placement.into_option().unwrap();
    let why = seen.explanation.as_option().unwrap();
    assert_eq!((seen.node.as_str(), seen.region.as_str()), ("", "eu"), "the region, not the node");
    assert_eq!(why.candidates, Some(1), "the tenant may count");
    assert!(why.scored.is_empty() && why.chosen.is_empty() && !why.policy.is_set());

    let seen = status(&h, owner, &w).await.placement.into_option().unwrap();
    assert_eq!(seen.tier.as_known(), Some(pb::IsolationTier::ISOLATION_TIER_CONTAINER));
    let counted = seen.explanation.as_option().unwrap().candidates;
    assert_eq!(counted, None, "one workload: no counts");

    let seen = status(&h, h.root(), &w).await.placement.into_option().unwrap();
    assert!(seen.explanation.as_option().unwrap().policy.is_set(), "the operator sees it all");
}

#[tokio::test]
async fn a_change_its_node_cannot_hold_is_unplaced_and_never_moved() {
    let h = Harness::new().await;
    let node = h.node(None, "fra-3", &[]).await;
    h.node(None, "nyc-1", &[]).await;
    let (w, repo) = workload(&h).await;
    let first = status(&h, h.root(), &w).await.placement.into_option().unwrap();
    let mut big = body(&repo, &[]);
    big.resources = pb::ResourceSpec {
        memory: pb::ReservedLimit { reservation: 64 * GIB, ..Default::default() }.into(),
        ..Default::default()
    }
    .into();
    let declared = h.workloads(h.root()).declare_intent(change("big", &w, 1, big)).await.unwrap();
    let plan = declared.into_owned().plan.into_option().unwrap();
    if plan.status.as_known() == Some(pb::PlanStatus::PLAN_STATUS_PROPOSED) {
        h.plans(h.root()).approve_plan(approve("a", &plan.id)).await.unwrap();
    }
    let s = status(&h, h.root(), &w).await;
    let p = s.placement.as_option().unwrap();
    assert_eq!((p.node.as_str(), p.intent.as_str()), (first.node.as_str(), first.intent.as_str()));
    assert_eq!(first.node, node, "it stays where it was");
    let why = s.unplaced.as_option().unwrap().explanation.as_option().unwrap();
    assert_eq!(why.candidates, Some(1), "only its own node is considered: moving is a migration");
    assert_eq!(why.filtered[0].check, "capacity");
    assert_eq!(s.convergence.as_known(), Some(pb::Convergence::CONVERGENCE_UNPLACED));
    let cancel = pb::CancelPlanRequest {
        mutation: mutation("x").into(),
        plan: plan.id.clone(),
        ..Default::default()
    };
    h.plans(h.root()).cancel_plan(cancel).await.unwrap();
    let s = status(&h, h.root(), &w).await;
    assert!(!s.unplaced.is_set(), "the withdrawn target's failure goes with it");
    assert_eq!(s.placement.as_option().unwrap().intent, first.intent);
    assert_ne!(s.convergence.as_known(), Some(pb::Convergence::CONVERGENCE_UNPLACED));
}

#[tokio::test]
async fn two_binds_racing_for_one_slot_never_both_win() {
    let h = Harness::new().await;
    let mut small =
        korpis_store::testing::ready_node(h.installed.organization.id, "fra-3", &[]).unwrap();
    small.allocatable.insert(korpis_core::tenancy::quota::Dimension::Memory, 6 * GIB);
    h.enroll(small).await;
    let repo = crate::recipes::push_paper().await;
    let project = crate::recipes::project(&h, &h.root_org(), "games").await;
    let create = |key: &'static str, name: &'static str| {
        let workloads = h.workloads(h.root());
        let req = crate::recipes::create(key, &project, name, body(&repo, &[]));
        async move { workloads.declare_intent(req).await.unwrap().into_owned() }
    };
    let (a, b) = tokio::join!(create("a", "one"), create("b", "two"));
    let mut placed_count = 0;
    for d in [a, b] {
        let s = status(&h, h.root(), &d.workload.as_option().unwrap().id).await;
        placed_count += usize::from(s.placement.is_set());
        assert!(s.placement.is_set() != s.unplaced.is_set(), "each is one or the other");
    }
    assert_eq!(placed_count, 1, "4 GiB each, 6 GiB free: the placement lock lets one win");
}

#[tokio::test]
async fn a_stable_endpoint_without_an_edge_tier_needs_a_node_reachable_on_its_own_address() {
    let h = Harness::new().await;
    let org = h.installed.organization.id;
    let mut behind_nat = korpis_store::testing::ready_node(org, "cgnat-1", &[]).unwrap();
    behind_nat.reachability = korpis_core::node::Reachability::EdgeOnly;
    h.enroll(behind_nat).await;
    let (w, _) = workload(&h).await;
    let s = status(&h, h.root(), &w).await;
    assert_eq!(s.convergence.as_known(), Some(pb::Convergence::CONVERGENCE_UNPLACED));
    let why = s.unplaced.as_option().unwrap().explanation.as_option().unwrap();
    let refusal = &why.filtered[0];
    assert_eq!(refusal.check, "reachability", "the recipe's game endpoint is stable");
    assert!(refusal.detail.contains("without an edge tier"), "{}", refusal.detail);
}
