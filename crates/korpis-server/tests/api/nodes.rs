// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nodes (§3.4 of docs/01-model.md): owned by the organization they were enrolled into, read
//! through it, and cordoned by the operator without touching what runs there.

use crate::support::{Harness, mutation, org_scope, reason};
use korpis_core::id::{Id, IdKind};
use korpis_core::principal::Principal;
use korpis_proto::korpis::v1 as pb;
use korpis_server::api::wire::principal;

async fn get(h: &Harness, id: &str) -> pb::Node {
    let nodes = h.nodes(h.root());
    let got = nodes.get_node(pb::GetNodeRequest { id: id.to_owned(), ..Default::default() });
    got.await.unwrap().into_owned().node.into_option().unwrap()
}

fn cordon(key: &str, node: &str) -> pb::CordonNodeRequest {
    pb::CordonNodeRequest {
        mutation: mutation(key).into(),
        node: node.to_owned(),
        ..Default::default()
    }
}

fn uncordon(key: &str, node: &str) -> pb::UncordonNodeRequest {
    pb::UncordonNodeRequest {
        mutation: mutation(key).into(),
        node: node.to_owned(),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_node_reads_as_enrolled_owned_by_its_organization() {
    let h = Harness::new().await;
    let id = h.node(None, "fra-3", &[("region", "eu")]).await;
    let n = get(&h, &id).await;
    assert_eq!((n.name.as_str(), n.organization.clone()), ("fra-3", h.root_org()));
    assert_eq!(n.status.as_known(), Some(pb::NodeStatus::NODE_STATUS_READY));
    assert_eq!(n.admin.as_known(), Some(pb::NodeAdmin::NODE_ADMIN_ACTIVE));
    assert_eq!(n.connectivity.as_known(), Some(pb::NodeConnectivity::NODE_CONNECTIVITY_CONNECTED));
    assert_eq!(n.capacity.as_option().unwrap().cpu_millis, 8_000);
    assert_eq!(n.allocated.as_option().unwrap().cpu_millis, 0, "nothing placed");
    let driver = &n.drivers[0];
    assert_eq!(driver.name, "oci");
    let caps = driver.capabilities.as_option().unwrap();
    assert_eq!(caps.tiers[0].as_known(), Some(pb::IsolationTier::ISOLATION_TIER_CONTAINER));
    assert!(caps.enforcement["memory"].enforced);
}

#[tokio::test]
async fn nodes_list_per_owner_and_selector() {
    let h = Harness::new().await;
    let eu = h.node(None, "fra-3", &[("region", "eu")]).await;
    h.node(None, "nyc-1", &[("region", "us")]).await;
    let nodes = h.nodes(h.root());
    let list = |selector: Option<pb::LabelSelector>| {
        nodes.list_nodes(pb::ListNodesRequest {
            organization: h.root_org(),
            selector: selector.into(),
            ..Default::default()
        })
    };
    assert_eq!(list(None).await.unwrap().into_owned().nodes.len(), 2);
    let only_eu = pb::LabelSelector {
        match_labels: [("region".to_owned(), "eu".to_owned())].into_iter().collect(),
        ..Default::default()
    };
    let listed = list(Some(only_eu)).await.unwrap().into_owned();
    assert_eq!(listed.nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>(), [eu]);
}

#[tokio::test]
async fn cordon_survives_nothing_but_uncordon_and_records_each_change_once() {
    let h = Harness::new().await;
    let id = h.node(None, "fra-3", &[]).await;
    let nodes = h.nodes(h.root());
    let n = nodes.cordon_node(cordon("c1", &id)).await.unwrap().into_owned().node.into_option();
    assert_eq!(n.unwrap().status.as_known(), Some(pb::NodeStatus::NODE_STATUS_CORDONED));
    nodes.cordon_node(cordon("c2", &id)).await.unwrap();
    assert_eq!(
        h.effects("node.cordon").await.len(),
        1,
        "a cordon of a cordoned node changes nothing"
    );
    let replay = nodes.cordon_node(cordon("c1", &id)).await.unwrap().into_owned();
    assert_eq!(replay.node.as_option().unwrap().id, id, "a retry replays");
    let n = nodes.uncordon_node(uncordon("u1", &id)).await.unwrap().into_owned();
    assert_eq!(
        n.node.as_option().unwrap().status.as_known(),
        Some(pb::NodeStatus::NODE_STATUS_READY)
    );
    assert_eq!(h.effects("node.uncordon").await, [("succeeded".to_owned(), Some(root_grant(&h)))]);
}

#[tokio::test]
async fn a_draining_node_is_not_uncordoned_by_a_toggle() {
    let h = Harness::new().await;
    let mut node =
        korpis_store::testing::ready_node(h.installed.organization.id, "fra-3", &[]).unwrap();
    node.admin = korpis_core::node::NodeAdmin::Draining;
    let id = h.enroll(node).await;
    let e = h.nodes(h.root()).uncordon_node(uncordon("u", &id)).await.unwrap_err();
    assert_eq!(reason(&e), "node.admin_transition");
}

#[tokio::test]
async fn reading_a_node_does_not_cordon_it_and_no_grant_sees_nothing() {
    let h = Harness::new().await;
    let id = h.node(None, "fra-3", &[]).await;
    let (token, secret) = h.bare_token().await;
    let none = h.config(Some(secret.expose()));
    let nodes = h.nodes(none.clone());
    let e = nodes.get_node(pb::GetNodeRequest { id: id.clone(), ..Default::default() });
    assert_eq!(reason(&e.await.unwrap_err()), "object.not_found", "existence is hidden");
    h.issue_to(token, "read", &["node.read"]).await;
    let e = h.nodes(none).cordon_node(cordon("c", &id)).await.unwrap_err();
    assert_eq!(reason(&e), "authorization.denied");
}

#[tokio::test]
async fn enrollment_tokens_wait_for_the_agent_protocol() {
    let h = Harness::new().await;
    let e = h
        .nodes(h.root())
        .create_enrollment_token(pb::CreateEnrollmentTokenRequest {
            mutation: mutation("t").into(),
            node_name: "fra-3".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(e.code, connectrpc::ErrorCode::Unimplemented);
}

fn root_grant(h: &Harness) -> uuid::Uuid {
    Id::parse(IdKind::Grant, &h.root_grant()).unwrap().uuid()
}

#[tokio::test]
async fn a_node_enrolled_into_a_customer_is_the_customers_and_its_providers() {
    let h = Harness::new().await;
    let created = h
        .orgs(h.root())
        .create_organization(pb::CreateOrganizationRequest {
            mutation: mutation("org").into(),
            name: "customer".into(),
            parent: Some(h.root_org()),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    let customer = created.organization.as_option().unwrap().id.clone();
    let customer_id = Id::parse(IdKind::Organization, &customer).unwrap().uuid();
    let theirs = h.node(Some(customer_id), "home-1", &[]).await;
    let providers = h.node(None, "fra-3", &[]).await;
    assert_eq!(get(&h, &theirs).await.organization, customer, "the provider sees below itself");
    let (token, secret) = h.bare_token().await;
    h.grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("g").into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Token(token)).into(),
            actions: vec!["node.read".into()],
            scope: org_scope(&customer).into(),
            ..Default::default()
        })
        .await
        .unwrap();
    let nodes = h.nodes(h.config(Some(secret.expose())));
    let read =
        |id: &str| nodes.get_node(pb::GetNodeRequest { id: id.to_owned(), ..Default::default() });
    assert!(read(&theirs).await.is_ok());
    assert_eq!(reason(&read(&providers).await.unwrap_err()), "object.not_found");
}
