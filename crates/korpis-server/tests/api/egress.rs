// SPDX-License-Identifier: AGPL-3.0-or-later

//! The control plane's API reached through the node (§9.5 of docs/08-identity.md): the control
//! plane marks the assignment of an installed extension's workload, and of a workload holding a
//! grant in force, and no other, as reaching its API, which the node relays to it. And what a
//! declaration may ask of the network: what no node enforces is refused (Rule K-3).

use crate::providers::installed;
use crate::recipes::{body, create, project, push_paper};
use crate::support::{Harness, detail, mutation, org_scope};
use buffa::Message as _;
use korpis_core::principal::Principal;
use korpis_proto::korpis::agent::v1 as agent;
use korpis_proto::korpis::v1 as pb;
use korpis_proto::korpis::v1::__buffa::oneof::network_rule::Peer;
use korpis_server::api::wire::principal;
use std::time::Duration;

/// The latest assignment of the workload with wire id `workload`, once it has one.
pub async fn assigned(h: &Harness, workload: &str) -> agent::Assignment {
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, workload).unwrap();
    let store = h.db.store().await.unwrap();
    for _ in 0..100 {
        let body: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT body FROM assignment WHERE workload_id = $1 ORDER BY generation DESC LIMIT 1",
        )
        .bind(id.uuid())
        .fetch_optional(store.pool())
        .await
        .unwrap();
        if let Some(b) = body {
            return agent::Assignment::decode_from_slice(&b).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{workload} was never assigned");
}

async fn reaches_api(h: &Harness, workload: &str) -> bool {
    assigned(h, workload).await.reaches_api
}

#[tokio::test]
async fn an_installed_extension_s_workload_reaches_the_api_and_no_other_does() {
    let h = Harness::new().await;
    let x = installed(&h, 41, "dns").await;
    assert!(reaches_api(&h, &x.extension.workload).await, "the extension's workload");

    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let declare = create("d1", &project, "mc", body(&repo, &[]));
    let out = h.workloads(h.root()).declare_intent(declare).await.unwrap().into_owned();
    let plain = out.workload.as_option().unwrap().id.clone();
    assert!(!reaches_api(&h, &plain).await, "a workload that is no extension");
}

/// Waits until the assignment of `workload` says `want` of reaching the API.
async fn turns(h: &Harness, workload: &str, want: bool) {
    for _ in 0..100 {
        if reaches_api(h, workload).await == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{workload} never came to reach the API: {want}");
}

#[tokio::test]
async fn a_workload_holding_a_grant_in_force_reaches_the_api_until_it_is_revoked() {
    let h = Harness::new().await;
    // An installation, so a node is connected and assignments are computed.
    let _connected = installed(&h, 41, "dns").await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "jobs").await;
    let declare = create("b1", &project, "nightly", body(&repo, &[]));
    let out = h.workloads(h.root()).declare_intent(declare).await.unwrap().into_owned();
    let nightly = out.workload.as_option().unwrap().id.clone();
    assert!(!reaches_api(&h, &nightly).await, "it holds nothing yet");

    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, &nightly).unwrap();
    let issued = h
        .grants(h.root())
        .issue_grant(pb::IssueGrantRequest {
            mutation: mutation("to-nightly").into(),
            parent: h.root_grant(),
            subject: principal::to_pb(&Principal::Workload(id.uuid())).into(),
            actions: vec!["backup.create".into(), "repository.read".into()],
            scope: org_scope(&h.root_org()).into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    turns(&h, &nightly, true).await;

    let grant = issued.grant.as_option().unwrap().id.clone();
    let revoke =
        pb::RevokeGrantRequest { mutation: mutation("r").into(), grant, ..Default::default() };
    h.grants(h.root()).revoke_grant(revoke).await.unwrap();
    turns(&h, &nightly, false).await;
}

/// Its organization suspended, a workload's grant scoped there suspends with it, and the node
/// stops relaying the API to it until the organization is resumed (§2.1 of
/// docs/08-identity.md). The grant is issued from the host's root, so the chain's root lies
/// outside the suspended organization and only its leaf within it.
#[tokio::test]
async fn a_workload_of_a_suspended_organization_loses_the_api_until_it_is_resumed() {
    let h = Harness::new().await;
    let _connected = installed(&h, 41, "dns").await;
    let made = pb::CreateOrganizationRequest {
        mutation: mutation("customer").into(),
        name: "customer".into(),
        parent: Some(h.root_org()),
        ..Default::default()
    };
    let customer = h.orgs(h.root()).create_organization(made).await.unwrap().into_owned();
    let customer = customer.organization.as_option().unwrap().id.clone();
    let project = project(&h, &customer, "jobs").await;
    let declare = create("b1", &project, "nightly", body(&push_paper().await, &[]));
    let out = h.workloads(h.root()).declare_intent(declare).await.unwrap().into_owned();
    let nightly = out.workload.as_option().unwrap().id.clone();
    let id = korpis_core::id::Id::parse(korpis_core::id::IdKind::Workload, &nightly).unwrap();
    let issue = pb::IssueGrantRequest {
        mutation: mutation("to-nightly").into(),
        parent: h.root_grant(),
        subject: principal::to_pb(&Principal::Workload(id.uuid())).into(),
        actions: vec!["backup.create".into()],
        scope: org_scope(&customer).into(),
        ..Default::default()
    };
    h.grants(h.root()).issue_grant(issue).await.unwrap();
    turns(&h, &nightly, true).await;

    let suspend = pb::SuspendOrganizationRequest {
        mutation: mutation("s").into(),
        organization: customer.clone(),
        reason: "unpaid".into(),
        ..Default::default()
    };
    h.orgs(h.root()).suspend_organization(suspend).await.unwrap();
    turns(&h, &nightly, false).await;
    let resume = pb::ResumeOrganizationRequest {
        mutation: mutation("r").into(),
        organization: customer,
        ..Default::default()
    };
    h.orgs(h.root()).resume_organization(resume).await.unwrap();
    turns(&h, &nightly, true).await;
}

#[tokio::test]
async fn a_declaration_asking_what_no_node_enforces_is_refused_saying_why() {
    let h = Harness::new().await;
    let repo = push_paper().await;
    let project = project(&h, &h.root_org(), "games").await;
    let endpoint = |exposure: pb::Exposure| pb::EndpointSpec {
        name: "game".into(),
        protocol: pb::EndpointProtocol::ENDPOINT_PROTOCOL_TCP.into(),
        container_port: 25565,
        exposure: exposure.into(),
        ..Default::default()
    };
    let refused = |b: pb::IntentBody, key: &str| {
        let declare = create(key, &project, key, b);
        let workloads = h.workloads(h.root());
        async move { workloads.declare_intent(declare).await.unwrap_err() }
    };

    // A dedicated address is built (§3.7 of docs/07-networking.md): declared, it waits for a
    // pool to reach a node with one free, which address_pools.rs tests.
    let mut dedicated = body(&repo, &[]);
    dedicated.endpoints = vec![endpoint(pb::Exposure::EXPOSURE_DEDICATED_IP)];
    let declared = create("dedicated", &project, "dedicated", dedicated);
    h.workloads(h.root()).declare_intent(declared).await.expect("declared");

    let mut outbound = body(&repo, &[]);
    let mut game = endpoint(pb::Exposure::EXPOSURE_NODE_BOUND);
    game.policy = pb::NetworkPolicy {
        egress: vec![pb::NetworkRule {
            action: pb::NetworkAction::NETWORK_ACTION_DENY.into(),
            peer: Some(Peer::Cidr("203.0.113.0/24".into())),
            ..Default::default()
        }],
        ..Default::default()
    }
    .into();
    outbound.endpoints = vec![game];
    let e = refused(outbound, "outbound").await;
    assert_eq!(detail(&e).field, "body.endpoints[0].policy.egress", "{e:?}");

    let mut raised = body(&repo, &[]);
    raised.egress = pb::EgressPolicy {
        limits: pb::ConntrackLimits { max_connections: 1 << 20, ..Default::default() }.into(),
        ..Default::default()
    }
    .into();
    let e = refused(raised, "raised").await;
    assert_eq!(detail(&e).field, "body.egress.limits", "{e:?}");
}
