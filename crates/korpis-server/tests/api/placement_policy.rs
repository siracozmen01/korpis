// SPDX-License-Identifier: AGPL-3.0-or-later

//! Placement policies per organization (§2.2 of docs/05-scheduling.md, question 8 of §10 there):
//! an organization sets one for its subtree under `org.write`, read under `org.read` with the one
//! in force and where it was set; a customer's workload lands on the node its own organization
//! owns before the host's, each scored by its owner's policy, which the explanation records.

use crate::delegation::{Holder, delegate, delegation, organization};
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, mutation, org_scope};
use connectrpc::ErrorCode;
use korpis_core::id::{Id, IdKind};
use korpis_proto::korpis::v1 as pb;
use uuid::Uuid;

fn policy(strategy: pb::PlacementStrategy) -> pb::PlacementPolicy {
    pb::PlacementPolicy { strategy: strategy.into(), ..Default::default() }
}

fn setting(org: &str, policy: Option<pb::PlacementPolicy>) -> pb::SetPlacementPolicyRequest {
    pb::SetPlacementPolicyRequest {
        mutation: mutation(&format!("policy-{}", Uuid::now_v7())).into(),
        organization: org.to_owned(),
        policy: policy.into(),
        ..Default::default()
    }
}

async fn at(h: &Harness, org: &str) -> pb::OrganizationPlacementPolicy {
    let req = pb::GetPlacementPolicyRequest { organization: org.to_owned(), ..Default::default() };
    let got = h.orgs(h.root()).get_placement_policy(req).await.unwrap().into_owned();
    got.policy.into_option().unwrap()
}

fn strategy(p: Option<&pb::PlacementPolicy>) -> Option<pb::PlacementStrategy> {
    p.and_then(|p| p.strategy.as_known())
}

#[tokio::test]
async fn a_customer_s_own_node_takes_its_workloads_each_scored_by_its_owner_s_policy() {
    let h = Harness::new().await;
    let host = Holder { grant: h.root_grant(), config: h.root(), token: Uuid::nil() };
    let root = h.root_org();
    let pinecone = organization(&h, &host, &root, "pinecone").await;
    let owned = Id::parse(IdKind::Organization, &pinecone).unwrap().uuid();
    let (own_node, host_node) =
        (h.node(Some(owned), "own-1", &[]).await, h.node(None, "host-1", &[]).await);

    let none = at(&h, &pinecone).await;
    assert!(!none.own.is_set() && none.set_at.is_empty(), "the installation's: {none:?}");
    let orgs = h.orgs(h.root());
    let spread = policy(pb::PlacementStrategy::PLACEMENT_STRATEGY_DISTRIBUTED);
    orgs.set_placement_policy(setting(&root, Some(spread))).await.unwrap();
    let inherited = at(&h, &pinecone).await;
    assert_eq!((inherited.set_at.as_str(), !inherited.own.is_set()), (root.as_str(), true));
    let pack = policy(pb::PlacementStrategy::PLACEMENT_STRATEGY_PACKED);
    let set = orgs.set_placement_policy(setting(&pinecone, Some(pack))).await.unwrap();
    let set = set.into_owned().policy.into_option().unwrap();
    assert_eq!(set.set_at, pinecone, "its own is in force for its subtree");
    assert_eq!(
        strategy(set.effective.as_option()),
        Some(pb::PlacementStrategy::PLACEMENT_STRATEGY_PACKED)
    );
    assert_eq!(h.effects("org.placement_policy.set").await.len(), 2);

    let repo = push_paper().await;
    let shop = project(&h, &pinecone, "shop").await;
    let workloads = h.workloads(h.root());
    let declared = workloads.declare_intent(create("w", &shop, "web", body(&repo, &[]))).await;
    let w = declared.unwrap().into_owned().workload.into_option().unwrap().id;
    let get = pb::GetWorkloadRequest { id: w, ..Default::default() };
    let got = workloads.get_workload(get).await.unwrap().into_owned();
    let placement = got.status.into_option().unwrap().placement.into_option().unwrap();
    assert_eq!(placement.node, own_node, "its own organization's node before the host's");
    let why = placement.explanation.into_option().unwrap();
    assert_eq!(why.runner_up.as_deref(), Some(host_node.as_str()), "the host's behind it");
    let by: Vec<(String, Option<pb::PlacementStrategy>)> = why
        .owner_policies
        .iter()
        .map(|o| (o.organization.clone(), strategy(o.policy.as_option())))
        .collect();
    assert!(
        by.contains(&(pinecone.clone(), Some(pb::PlacementStrategy::PLACEMENT_STRATEGY_PACKED)))
    );
    assert!(
        by.contains(&(root.clone(), Some(pb::PlacementStrategy::PLACEMENT_STRATEGY_DISTRIBUTED)))
    );

    orgs.set_placement_policy(setting(&pinecone, None)).await.unwrap();
    assert_eq!(at(&h, &pinecone).await.set_at, root, "removed, the root's is in force again");
}

#[tokio::test]
async fn a_policy_is_set_under_org_write_over_the_organization_and_read_under_org_read() {
    let h = Harness::new().await;
    let host = Holder { grant: h.root_grant(), config: h.root(), token: Uuid::nil() };
    let pinecone = organization(&h, &host, &h.root_org(), "pinecone").await;
    let ask = (&["org.read"][..], org_scope(&pinecone));
    let reader = delegate(&h, (&h.root_grant(), h.root()), ask, delegation(None, &[])).await;
    let orgs = h.orgs(reader.config.clone());
    let req =
        pb::GetPlacementPolicyRequest { organization: pinecone.clone(), ..Default::default() };
    orgs.get_placement_policy(req).await.expect("read under org.read");
    let pack = policy(pb::PlacementStrategy::PLACEMENT_STRATEGY_PACKED);
    let refused = orgs.set_placement_policy(setting(&pinecone, Some(pack.clone()))).await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::PermissionDenied, "not without org.write");
    let above = orgs.set_placement_policy(setting(&h.root_org(), Some(pack))).await;
    assert_eq!(above.unwrap_err().code, ErrorCode::NotFound, "nor over an organization above");
}
