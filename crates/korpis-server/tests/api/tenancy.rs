// SPDX-License-Identifier: AGPL-3.0-or-later

//! Server information, authentication, and tenancy.

use crate::support::{Harness, mutation, reason};
use connectrpc::ErrorCode;
use korpis_proto::korpis::v1 as pb;

#[tokio::test]
async fn server_info_needs_no_credential() {
    let h = Harness::new().await;
    let info = pb::ServerInfoServiceClient::new(h.http.clone(), h.config(None))
        .get_server_info(pb::GetServerInfoRequest::default())
        .await
        .expect("server info")
        .into_owned();
    assert_eq!(info.version, korpis_core::build::VERSION);
    assert!(info.agent_protocols.iter().any(|p| p == "korpis.agent.v1"));
}

#[tokio::test]
async fn calls_without_a_credential_are_unauthenticated() {
    let h = Harness::new().await;
    let root = h.root_org();
    for config in [h.config(None), h.config(Some("korpis_not-a-real-token"))] {
        let e = h
            .orgs(config)
            .get_organization(pb::GetOrganizationRequest { id: root.clone(), ..Default::default() })
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Unauthenticated);
    }
}

#[tokio::test]
async fn root_token_creates_and_replays() {
    let h = Harness::new().await;
    let root = h.root_org();
    let orgs = h.orgs(h.root());
    let got = orgs
        .get_organization(pb::GetOrganizationRequest { id: root.clone(), ..Default::default() })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(got.organization.as_option().unwrap().name, "acme");

    let create = |key: &str| pb::CreateOrganizationRequest {
        mutation: mutation(key).into(),
        name: "customer-a".into(),
        parent: Some(root.clone()),
        ..Default::default()
    };
    let first = orgs.create_organization(create("k1")).await.unwrap().into_owned();
    let again = orgs.create_organization(create("k1")).await.unwrap().into_owned();
    let child = first.organization.as_option().unwrap().id.clone();
    assert_eq!(child, again.organization.as_option().unwrap().id, "a retry replays");
    // A new key is a new request, and the name is taken.
    let e = orgs.create_organization(create("k2")).await.unwrap_err();
    assert_eq!(reason(&e), "organization.name_taken");

    let project = h
        .projects(h.root())
        .create_project(pb::CreateProjectRequest {
            mutation: mutation("k3").into(),
            organization: child.clone(),
            name: "web".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_owned();
    assert_eq!(project.project.as_option().unwrap().organization, child);

    // Every change is an effect naming the root grant.
    for action in ["org.create", "project.create"] {
        let rows = h.effects(action).await;
        assert!(
            rows.iter().any(|(o, g)| o == "succeeded" && *g == Some(h.installed.root_grant)),
            "{action}: {rows:?}"
        );
    }
}

#[tokio::test]
async fn an_idempotency_key_answers_one_operation_only() {
    let h = Harness::new().await;
    let root = h.root_org();
    h.orgs(h.root())
        .create_organization(pb::CreateOrganizationRequest {
            mutation: mutation("same").into(),
            name: "customer-b".into(),
            parent: Some(root.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    let e = h
        .projects(h.root())
        .create_project(pb::CreateProjectRequest {
            mutation: mutation("same").into(),
            organization: root,
            name: "web".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "idempotency.key_reused");
    assert!(h.effects("project.create").await.is_empty(), "nothing happened");
}

#[tokio::test]
async fn malformed_input_is_refused_before_anything_happens() {
    let h = Harness::new().await;
    let e = h
        .projects(h.root())
        .create_project(pb::CreateProjectRequest {
            mutation: mutation("p1").into(),
            organization: h.root_org(),
            name: "web".into(),
            labels: [("-bad".to_owned(), "x".to_owned())].into_iter().collect(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    let e = h
        .orgs(h.root())
        .get_organization(pb::GetOrganizationRequest {
            id: "prj_00000000000000000000000000000000".into(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(reason(&e), "request.invalid", "a project id is not an organization id");
}
